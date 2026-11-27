// 使用私有回环信令实例和临时身份，验证上游连接、结束与凭据生命周期。
import assert from 'node:assert/strict';
import { generateKeyPairSync, sign } from 'node:crypto';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { test } from 'node:test';

const port = Number(process.env.REMOTEOPS_CROSSDESK_TEST_PORT);
const address = `wss://127.0.0.1:${port}`;

async function connect(cleanup) {
    const socket = new WebSocket(address);
    cleanup.push(socket);
    await once(socket, 'open', { signal: AbortSignal.timeout(5000) });
    return socket;
}

function nextMessage(socket, type) {
    return new Promise((resolve, reject) => {
        const finish = () => {
            clearTimeout(timer);
            socket.removeEventListener('message', message);
            socket.removeEventListener('close', closed);
        };
        function closed() {
            finish();
            reject(new Error(`等待 ${type} 时连接关闭`));
        }
        function message(event) {
            const body = JSON.parse(event.data);
            if (body.type !== type) return;
            finish();
            resolve(body);
        }
        const timer = setTimeout(() => {
            finish();
            reject(new Error(`等待 ${type} 超时`));
        }, 10000);
        socket.addEventListener('message', message);
        socket.addEventListener('close', closed);
    });
}

function sendAndWait(socket, request, response, recipient = socket) {
    const received = nextMessage(recipient, response);
    socket.send(JSON.stringify(request));
    return received;
}

test('私有 CrossDesk 信令支持独立现场身份、临时控制端和按连接断开', { timeout: 45000 }, async () => {
    assert.ok(Number.isInteger(port) && port > 1023 && port < 65536, '需要隔离实例的端口');
    const sockets = [];
    try {
        const host = await connect(sockets);
        const pair = generateKeyPairSync('ed25519');
        const key = Buffer.from(pair.publicKey.export({ format: 'jwk' }).x, 'base64url').toString('hex');
        const login = { type: 'login', user_id: '', device_key: { version: 1, public_key: key } };
        const challenge = await sendAndWait(host, login, 'login_challenge');
        assert.match(challenge.nonce, /^[a-f0-9]{64}$/);
        const transcript = JSON.stringify(['crossdesk.device-login.v1', '', key, challenge.nonce]);
        const signature = sign(null, Buffer.from(transcript), pair.privateKey).toString('hex');
        const registered = await sendAndWait(host, {
            ...login, device_key: { ...login.device_key, nonce: challenge.nonce, signature },
        }, 'login');
        assert.ok(registered.status === 'success' && /^\d{9}@[^@]+$/.test(registered.user_id),
            '现场身份登记失败');
        const device = registered.user_id.split('@')[0];
        const guest = await connect(sockets);
        const identity = await sendAndWait(guest, { type: 'login', user_id: 'cli' }, 'login');
        const guestId = identity.user_id?.split('@')[0];
        assert.ok(identity.status === 'success' && /^cli-\d{9}$/.test(guestId), '临时控制端登记失败');
        const joined = await sendAndWait(guest, {
            type: 'join_transmission', transmission_id: registered.user_id, user_id: guestId,
        }, 'user_join_transmission', host);
        assert.ok(joined.status === 'success' && joined.user_id === guestId, '现场未收到对应加入事件');
        const left = await sendAndWait(host, {
            type: 'disconnect_peer', transmission_id: device, user_id: device, remote_user_id: guestId,
        }, 'user_leave_transmission', guest);
        assert.ok(left.transmission_id === device, '退出事件不属于该会话');
    } finally {
        for (const socket of sockets) socket.close();
        await delay(50);
    }
});

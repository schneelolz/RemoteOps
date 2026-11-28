import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import test from 'node:test';

// 两个独立的媒体进程只通过与产品一致的有界管道收发。口令不写日志。
function peer(sender, ticket) {
  const child = spawn(process.env.REMOTEOPS_CROSSDESK_WORKER, [], {
    windowsHide: true, stdio: ['pipe', 'pipe', 'ignore'],
    cwd: process.env.REMOTEOPS_CROSSDESK_LAB_DIRECTORY,
  });
  const events = [];
  let buffered = Buffer.alloc(0);
  let fault;
  child.on('error', e => { fault = e; });
  child.stdin.on('error', e => { fault = e; });
  child.stdout.on('data', next => {
    buffered = Buffer.concat([buffered, next]);
    while (buffered.length >= 5) {
      const type = String.fromCharCode(buffered[0]);
      const size = buffered.readUInt32LE(1);
      if (size > 2 * 1024 * 1024) { fault = new Error('媒体输出帧过大'); return; }
      if (buffered.length < 5 + size) return;
      events.push([type, buffered.subarray(5, 5 + size)]);
      buffered = buffered.subarray(5 + size);
    }
  });
  function write(type, data = Buffer.alloc(0)) {
    const header = Buffer.alloc(5);
    header[0] = type.charCodeAt(0);
    header.writeUInt32LE(data.length, 1);
    child.stdin.write(Buffer.concat([header, data]));
  }
  write('I', Buffer.from(JSON.stringify({ sender, ticket, host: '127.0.0.1',
    port: Number(process.env.REMOTEOPS_CROSSDESK_TEST_PORT),
    log_directory: process.env.REMOTEOPS_CROSSDESK_LAB_DIRECTORY,
    force_turn: process.env.REMOTEOPS_CROSSDESK_FIXTURE_TURN === '1' })));
  const heartbeat = setInterval(() => write('H'), 600);
  async function next(wanted) {
    const until = Date.now() + 22000;
    while (Date.now() < until) {
      if (fault) throw fault;
      const item = events.shift();
      if (item) {
        if (item[0] === wanted) return item[1];
        assert.notEqual(item[0], 'E', '媒体组件提前结束');
      } else {
        assert.equal(child.exitCode, null, '媒体组件异常退出');
        await delay(8);
      }
    }
    throw new Error('等待媒体组件消息超时 ' + wanted);
  }
  async function close() {
    clearInterval(heartbeat);
    if (child.exitCode !== null || child.signalCode !== null) return;
    const done = once(child, 'exit');
    child.kill();
    await done;
  }
  return { write, next, close, stopHeartbeat: () => clearInterval(heartbeat) };
}

test('已授权的两个产品媒体进程收到真实 H.264，失去控制链租约后关闭', { timeout: 55000 }, async () => {
  const ticket = randomUUID();
  const host = peer(true, ticket);
  const viewer = peer(false, ticket);
  let frames;
  try {
    viewer.write('S', await host.next('S'));
    await host.next('R');
    const frame = Buffer.alloc(8 + 1280 * 720 * 4, 140);
    frame.writeUInt32LE(1280); frame.writeUInt32LE(720, 4);
    frames = setInterval(() => host.write('F', frame), 100);
    await viewer.next(process.env.REMOTEOPS_CROSSDESK_FIXTURE_TURN === '1' ? 'T' : 'P');
    const encoded = await viewer.next('V');
    assert.ok(encoded.length > 16 && encoded.indexOf(Buffer.from([0, 0, 1])) >= 0, '缺少 H.264 Annex B');
    clearInterval(frames);
    host.stopHeartbeat();
    await host.next('E');
  } finally {
    clearInterval(frames);
    await Promise.all([host.close(), viewer.close()]);
  }
});

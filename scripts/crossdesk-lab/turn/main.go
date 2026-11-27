// 仅用于回环 TURN/UDP 验收，监听、分配和目标地址都限于本机。
package main

import (
	"fmt"
	"io"
	"net"
	"os"
	"sync/atomic"

	"github.com/pion/logging"
	"github.com/pion/turn/v4"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	secret := os.Getenv("REMOTEOPS_FIXTURE_TURN_SECRET")
	if len(secret) < 32 {
		return fmt.Errorf("需要本次测试的临时密钥")
	}
	const address = "127.0.0.1"
	socket, err := net.ListenPacket("udp4", address+":0")
	if err != nil {
		return err
	}
	defer socket.Close()
	logs := logging.NewDefaultLoggerFactory()
	logs.Writer = io.Discard
	var auth, permissions atomic.Uint32
	verify := turn.LongTermTURNRESTAuthHandler(secret, logs.NewLogger("turn"))
	server, err := turn.NewServer(turn.ServerConfig{
		Realm: "remoteops-fixture", LoggerFactory: logs,
		AuthHandler: func(user, realm string, client net.Addr) ([]byte, bool) {
			key, ok := verify(user, realm, client)
			if ok {
				auth.Add(1)
			}
			return key, ok
		},
		PacketConnConfigs: []turn.PacketConnConfig{{
			PacketConn:            socket,
			RelayAddressGenerator: &turn.RelayAddressGeneratorStatic{RelayAddress: net.ParseIP(address), Address: address},
			PermissionHandler: func(_ net.Addr, target net.IP) bool {
				if !target.Equal(net.ParseIP(address)) {
					return false
				}
				permissions.Add(1)
				return true
			},
		}},
	})
	if err != nil {
		return err
	}
	defer server.Close()
	fmt.Println(socket.LocalAddr().(*net.UDPAddr).Port)
	_, err = io.Copy(io.Discard, os.Stdin)
	fmt.Fprintf(os.Stderr, "TURN authenticated requests: %d, allowed loopback permissions: %d\n", auth.Load(), permissions.Load())
	return err
}

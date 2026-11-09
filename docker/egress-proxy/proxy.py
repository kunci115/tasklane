#!/usr/bin/env python3
"""A tiny egress allowlist proxy for the tasklane agent sandbox.

The agent container sits on a Docker network that has no route to the internet. Its only way out is this
proxy, which accepts HTTPS `CONNECT host:443` for hosts on an allowlist and refuses everything else
(plain HTTP, other ports, other hosts, and any host that resolves to a private, loopback or link-local
address). It never looks inside the TLS stream. Python standard library only.

  ALLOW   comma-separated host names (default: api.anthropic.com)
  LISTEN  address to listen on (default: 0.0.0.0:3128)
"""
import ipaddress
import os
import select
import socket
import socketserver
import sys

ALLOW = {h.strip().lower() for h in os.environ.get("ALLOW", "api.anthropic.com").split(",") if h.strip()}
LISTEN = os.environ.get("LISTEN", "0.0.0.0:3128")
IDLE = 300


def public_address(host, port):
    """First address of `host` that is not private, loopback or link-local; None if there is none."""
    try:
        infos = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    except OSError:
        return None
    for family, _, _, _, sockaddr in infos:
        ip = ipaddress.ip_address(sockaddr[0])
        if ip.is_global:
            return family, sockaddr
    return None


class Handler(socketserver.StreamRequestHandler):
    def reply(self, status):
        self.wfile.write(("HTTP/1.1 %s\r\nConnection: close\r\n\r\n" % status).encode())

    def handle(self):
        try:
            line = self.rfile.readline(4096).decode("latin-1").strip()
            while True:  # discard the headers
                h = self.rfile.readline(8192)
                if h in (b"\r\n", b"\n", b""):
                    break
            parts = line.split()
            if len(parts) < 2 or parts[0].upper() != "CONNECT":
                return self.reply("405 Method Not Allowed")
            host, _, port = parts[1].rpartition(":")
            host = host.strip("[]").lower()
            if port != "443" or host not in ALLOW:
                print("refused %s" % parts[1], file=sys.stderr, flush=True)
                return self.reply("403 Forbidden")
            target = public_address(host, 443)
            if not target:
                print("refused %s (no public address)" % host, file=sys.stderr, flush=True)
                return self.reply("403 Forbidden")
            upstream = socket.socket(target[0], socket.SOCK_STREAM)
            upstream.settimeout(15)
            try:
                upstream.connect(target[1])
            except OSError:
                upstream.close()
                return self.reply("502 Bad Gateway")
            self.wfile.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            self.pipe(self.connection, upstream)
        except OSError:
            pass

    def pipe(self, a, b):
        socks = [a, b]
        b.settimeout(None)
        while True:
            r, _, x = select.select(socks, [], socks, IDLE)
            if x or not r:
                break
            for s in r:
                data = s.recv(65536)
                if not data:
                    b.close()
                    return
                (b if s is a else a).sendall(data)


class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    host, _, port = LISTEN.rpartition(":")
    print("egress proxy on %s, allowing: %s" % (LISTEN, ", ".join(sorted(ALLOW))), file=sys.stderr, flush=True)
    Server((host, int(port)), Handler).serve_forever()

#!/usr/bin/env node
// A tiny MCP server (stdio) that runs next to the agent and forwards tool calls to the tasklane broker over a
// Unix socket. It has no network access of its own and no credential: the broker does the reading.
// Environment: TL_SOCK (socket path), TL_TOKEN (per-run token).
"use strict";
const net = require("net");
const readline = require("readline");

const SOCK = process.env.TL_SOCK;
const TOKEN = process.env.TL_TOKEN;

function ask(req) {
  return new Promise((resolve) => {
    const c = net.createConnection(SOCK);
    let buf = "";
    c.on("connect", () => c.write(JSON.stringify(Object.assign({ token: TOKEN }, req)) + "\n"));
    c.on("data", (d) => {
      buf += d.toString();
      const i = buf.indexOf("\n");
      if (i >= 0) {
        c.end();
        try { resolve(JSON.parse(buf.slice(0, i))); } catch (e) { resolve({ ok: false, error: "bad answer" }); }
      }
    });
    c.on("error", () => resolve({ ok: false, error: "the tool broker is not reachable" }));
  });
}

function send(msg) { process.stdout.write(JSON.stringify(msg) + "\n"); }

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", async (line) => {
  let m;
  try { m = JSON.parse(line); } catch (e) { return; }
  if (m.id === undefined) return; // notifications need no answer
  if (m.method === "initialize") {
    const v = (m.params && m.params.protocolVersion) || "2024-11-05";
    return send({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: v, capabilities: { tools: {} }, serverInfo: { name: "tasklane", version: "1" } } });
  }
  if (m.method === "ping") return send({ jsonrpc: "2.0", id: m.id, result: {} });
  if (m.method === "tools/list") {
    const r = await ask({ op: "list" });
    return send({ jsonrpc: "2.0", id: m.id, result: { tools: r.ok ? r.tools : [] } });
  }
  if (m.method === "tools/call") {
    const p = m.params || {};
    const r = await ask({ op: "call", tool: p.name, args: p.arguments || {} });
    return send({ jsonrpc: "2.0", id: m.id, result: { content: [{ type: "text", text: r.ok ? r.text : "error: " + r.error }], isError: !r.ok } });
  }
  send({ jsonrpc: "2.0", id: m.id, error: { code: -32601, message: "method not found" } });
});

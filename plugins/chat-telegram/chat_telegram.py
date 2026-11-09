#!/usr/bin/env python3
"""Chat channel plug-in for Telegram (Bot API over HTTPS, standard library only).

Protocol: one JSON object per line on stdin; one response per line on stdout.
Role: chat_channel (receive, send, leave). `send` also answers the notifier
shape {recipient_ref, text, link?} when recipient_ref is a numeric chat id.

Env: TELEGRAM_BOT_TOKEN (secret), TELEGRAM_API_URL (default
https://api.telegram.org), TELEGRAM_SKIP_BACKLOG=1 (drop messages older than
the process start).

The bot token is part of the Bot API URL path, so URLs are never logged and
never put into error messages; every outgoing message is also scrubbed.
"""
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

PROTOCOL_MAJOR = 1
VERSION = "0.1.0"
MAX_TEXT = 4096          # Telegram limit, in UTF-16 code units
MAX_CALLBACK_BYTES = 64
MAX_POLL_SECS = 50
ALLOWED_UPDATES = ["message", "callback_query", "my_chat_member", "chat_member"]
COMMAND_RE = re.compile(r"^/(ask|task)(?:@(\w+))?(?:\s+(.*))?$", re.S)
GONE = ("left", "kicked")

START_TIME = time.time()


class PluginError(Exception):
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind
        self.message = message


def token():
    return os.environ.get("TELEGRAM_BOT_TOKEN", "")


def scrub(text):
    t = token()
    text = str(text)
    return text.replace(t, "***") if t else text


class State:
    offset = None   # next getUpdates offset
    me = None       # bot identity from getMe


def api(method, params=None, timeout=30):
    """Call a Bot API method. Raises PluginError; never echoes the URL."""
    tok = token()
    if not tok:
        raise PluginError("unauthorized", "TELEGRAM_BOT_TOKEN is not set")
    base = os.environ.get("TELEGRAM_API_URL", "https://api.telegram.org").rstrip("/")
    req = urllib.request.Request(
        f"{base}/bot{tok}/{method}",
        data=json.dumps(params or {}).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read()
            status = resp.status
    except urllib.error.HTTPError as e:
        try:
            raw = e.read()
        except Exception:
            raw = b""
        raise http_error(e.code, raw) from None
    except Exception as e:  # network, DNS, TLS, timeout
        raise PluginError("transient", scrub(f"network error calling {method}: {type(e).__name__}")) from None
    try:
        body = json.loads(raw)
    except ValueError:
        raise PluginError("transient", f"{method}: invalid response (http {status})") from None
    if not isinstance(body, dict) or not body.get("ok"):
        code = body.get("error_code", status) if isinstance(body, dict) else status
        raise http_error(code if isinstance(code, int) else 400, raw)
    return body.get("result")


def http_error(code, raw):
    desc, retry = "", None
    try:
        body = json.loads(raw)
        desc = str(body.get("description", ""))[:200]
        retry = (body.get("parameters") or {}).get("retry_after")
    except Exception:
        pass
    detail = f"telegram http {code}" + (f": {desc}" if desc else "")
    if code == 401:
        return PluginError("unauthorized", scrub(detail))
    if code == 429:
        extra = f" (retry_after={retry}s)" if retry is not None else ""
        return PluginError("rate_limited", scrub(detail + extra))
    if code >= 500 or code == 409:
        return PluginError("transient", scrub(detail))
    return PluginError("permanent", scrub(detail))


def get_me():
    if State.me is None:
        State.me = api("getMe") or {}
    return State.me


# ---------------------------------------------------------------- receive

def user_fields(u):
    u = u or {}
    name = u.get("username") or u.get("first_name") or ""
    return {"user": str(u.get("id", "")), "user_name": name}


def command_from(text, bot_username):
    m = COMMAND_RE.match(text.strip())
    if not m:
        return None
    target = m.group(2)
    if target and bot_username and target.lower() != bot_username.lower():
        return None  # addressed to another bot
    return {"command": m.group(1), "args": (m.group(3) or "").strip()}


def member_event(kind, chat, user, message_id=""):
    ev = {"kind": kind, "group": str(chat.get("id", "")), "message_id": message_id}
    ev.update(user_fields(user))
    return ev


def normalize(update, me):
    """Turn one Telegram update into a list of events."""
    out = []
    bot_id = me.get("id")
    skip_old = os.environ.get("TELEGRAM_SKIP_BACKLOG") == "1"
    msg = update.get("message")
    if isinstance(msg, dict):
        chat = msg.get("chat") or {}
        if skip_old and msg.get("date", START_TIME) < START_TIME:
            return out
        mid = str(msg.get("message_id", ""))
        for u in msg.get("new_chat_members") or []:
            if u.get("id") != bot_id:  # the bot's own join comes from my_chat_member
                out.append(member_event("joined", chat, u, mid))
        left = msg.get("left_chat_member")
        if left and left.get("id") != bot_id:
            out.append(member_event("left", chat, left, mid))
        text = msg.get("text")
        if isinstance(text, str) and text:
            ev = {"kind": "message", "group": str(chat.get("id", "")),
                  "text": text, "message_id": mid}
            ev.update(user_fields(msg.get("from")))
            cmd = command_from(text, me.get("username"))
            if cmd:
                ev.update(kind="command", **cmd)
            out.append(ev)
    cb = update.get("callback_query")
    if isinstance(cb, dict):
        cmsg = cb.get("message") or {}
        ev = {"kind": "button", "group": str((cmsg.get("chat") or {}).get("id", "")),
              "data": cb.get("data", ""), "message_id": str(cmsg.get("message_id", ""))}
        ev.update(user_fields(cb.get("from")))
        out.append(ev)
    for key in ("my_chat_member", "chat_member"):
        upd = update.get(key)
        if not isinstance(upd, dict):
            continue
        chat = upd.get("chat") or {}
        if chat.get("type") == "private":
            continue
        old = (upd.get("old_chat_member") or {})
        new = (upd.get("new_chat_member") or {})
        was_in = old.get("status") not in GONE and not (
            old.get("status") == "restricted" and not old.get("is_member", True))
        is_in = new.get("status") not in GONE and not (
            new.get("status") == "restricted" and not new.get("is_member", True))
        if is_in and not was_in:
            out.append(member_event("joined", chat, new.get("user")))
        elif was_in and not is_in:
            out.append(member_event("left", chat, new.get("user")))
    return out


def op_receive(params):
    try:
        secs = int(params.get("timeout_secs", 0))
    except (TypeError, ValueError):
        raise PluginError("permanent", "timeout_secs must be an integer") from None
    secs = max(0, min(secs, MAX_POLL_SECS))
    try:
        me = get_me()
    except PluginError as e:
        if e.kind in ("unauthorized", "permanent"):
            raise
        me = {}  # identity is best effort; retry on the next call
    req = {"timeout": secs, "allowed_updates": ALLOWED_UPDATES}
    if State.offset is not None:
        req["offset"] = State.offset
    updates = api("getUpdates", req, timeout=secs + 10) or []
    events, callback_ids = [], []
    for upd in updates:
        if not isinstance(upd, dict):
            continue
        uid = upd.get("update_id")
        if isinstance(uid, int):
            State.offset = max(State.offset or 0, uid + 1)
        try:
            events.extend(normalize(upd, me))
        except Exception:
            continue  # one malformed update must not drop the batch
        cb = upd.get("callback_query")
        if isinstance(cb, dict) and cb.get("id"):
            callback_ids.append(cb["id"])
    for cid in callback_ids:  # stop the client-side spinner
        try:
            api("answerCallbackQuery", {"callback_query_id": cid})
        except PluginError:
            pass
    seen, unique = set(), []
    for ev in events:  # service message and chat_member may describe the same change
        if ev["kind"] in ("joined", "left"):
            key = (ev["kind"], ev["group"], ev["user"])
            if key in seen:
                continue
            seen.add(key)
        unique.append(ev)
    return {"events": unique}


# ------------------------------------------------------------------- send

def utf16_len(s):
    return len(s.encode("utf-16-le")) // 2


def split_text(text):
    chunks = []
    while utf16_len(text) > MAX_TEXT:
        cut = MAX_TEXT
        while utf16_len(text[:cut]) > MAX_TEXT:
            cut -= 1
        nl = text.rfind("\n", cut * 3 // 4, cut)
        if nl > 0:
            cut = nl + 1
        chunks.append(text[:cut])
        text = text[cut:]
    chunks.append(text)
    return chunks


def keyboard(buttons):
    rows = []
    for b in buttons:
        if not isinstance(b, dict) or not isinstance(b.get("label"), str) \
                or not isinstance(b.get("data"), str):
            raise PluginError("permanent", "each button needs string label and data")
        if len(b["data"].encode()) > MAX_CALLBACK_BYTES:
            raise PluginError("permanent", f"button data longer than {MAX_CALLBACK_BYTES} bytes")
        rows.append([{"text": b["label"], "callback_data": b["data"]}])
    return {"inline_keyboard": rows}


def send_text(chat_id, text, buttons=None):
    if not isinstance(text, str) or not text:
        raise PluginError("permanent", "text must be a non-empty string")
    markup = keyboard(buttons) if buttons else None
    chunks = split_text(text)
    first = None
    for i, chunk in enumerate(chunks):
        req = {"chat_id": chat_id, "text": chunk, "disable_web_page_preview": True}
        if markup and i == len(chunks) - 1:
            req["reply_markup"] = markup
        res = api("sendMessage", req) or {}
        if first is None:
            first = res.get("message_id")
    return first


def chat_id_of(value):
    s = str(value).strip()
    if not re.fullmatch(r"-?\d+", s):
        raise PluginError("permanent", "group must be a numeric chat id")
    return int(s)


def op_send(params):
    if "group" in params:
        mid = send_text(chat_id_of(params["group"]), params.get("text"), params.get("buttons"))
        return {"message_id": str(mid)}
    if "recipient_ref" in params:
        text = params.get("text")
        if isinstance(text, str) and params.get("link"):
            text = f"{text}\n{params['link']}"
        send_text(chat_id_of(params["recipient_ref"]), text)
        return {"delivered": True}
    raise PluginError("permanent", "send needs group+text or recipient_ref+text")


def op_leave(params):
    api("leaveChat", {"chat_id": chat_id_of(params.get("group"))})
    return {}


# ------------------------------------------------------------------- main

def handle(req):
    rid, op = req.get("id"), req.get("op")
    params = req.get("params")
    if not isinstance(params, dict):
        params = {}
    try:
        if op == "describe":
            result = {"name": "telegram", "role": "chat_channel",
                      "protocol_major": PROTOCOL_MAJOR, "version": VERSION}
        elif op == "receive":
            result = op_receive(params)
        elif op == "send":
            result = op_send(params)
        elif op == "leave":
            result = op_leave(params)
        else:
            raise PluginError("permanent", f"unknown op: {op}")
        return {"id": rid, "ok": True, "result": result}
    except PluginError as e:
        return {"id": rid, "ok": False, "error": {"kind": e.kind, "message": e.message}}
    except Exception as e:
        return {"id": rid, "ok": False, "error": {
            "kind": "transient", "message": scrub(f"internal error: {type(e).__name__}")}}


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            if not isinstance(req, dict):
                raise ValueError("request must be an object")
            resp = handle(req)
        except ValueError:
            resp = {"id": None, "ok": False, "error": {"kind": "permanent", "message": "bad json line"}}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()

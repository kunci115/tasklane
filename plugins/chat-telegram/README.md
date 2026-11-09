# chat-telegram

Chat channel plug-in for Telegram (role `chat_channel`). Python 3.10+, standard library only. Protocol: JSON lines on stdin/stdout (see `docs/design/05-plugin-protocol.md`, `06-plugin-operations.md`).

## Environment

| Variable | Meaning |
|---|---|
| `TELEGRAM_BOT_TOKEN` | Bot token (secret). Never printed or put in errors. |
| `TELEGRAM_API_URL` | Default `https://api.telegram.org`; override for tests. |
| `TELEGRAM_SKIP_BACKLOG` | `1` drops messages older than the process start. Default off. |

## Create the bot

1. In Telegram, talk to `@BotFather`, send `/newbot`, pick a name and a username; copy the token.
2. Send `/setprivacy`, choose the bot, then **Disable**. With privacy mode on, a bot in a group only sees commands and replies; disabling lets it see all messages. Keep it on if you only need `/ask` and `/task` (commands, including `/ask@botname`, are still delivered). Remove and re-add the bot to groups after changing it.
3. Add the bot to a group. To receive member changes of other users, make the bot an admin (Telegram only sends `chat_member` updates to admins).

## Operations

- `describe` -> `{name: "telegram", role: "chat_channel", protocol_major: 1, version}`
- `receive {timeout_secs}` -> `{events: [...]}`. Long poll (`getUpdates`, max 50 s). The offset is kept in memory and advanced when updates are returned, so an update is delivered once per process.
- `send {group, text, buttons?: [{label, data}]}` -> `{message_id}` (first message). Plain text, no parse mode. Text over 4096 characters is split; buttons go on the last part. `data` over 64 bytes is a permanent error.
- `send {recipient_ref, text, link?}` (notifier shape, numeric chat id) -> `{delivered: true}`.
- `leave {group}` -> `{}`.

## Events

Every event has `kind`, `group` (chat id), `user` (numeric user id, string), plus extras `user_name` (username or first name) and `message_id`.

| kind | When | Extra fields |
|---|---|---|
| `message` | plain text | `text` |
| `command` | text starts with `/ask` or `/task` (also `/ask@thisbot`) | `text`, `command`, `args` |
| `button` | inline button pressed (the spinner is stopped with `answerCallbackQuery`) | `data` |
| `joined` | user added (service message or `chat_member`), or the bot itself added (`my_chat_member`, `user` = bot id) | |
| `left` | user left or removed, or the bot removed | |

Commands addressed to another bot (`/ask@other_bot`) are delivered as plain messages.

## Errors

401 -> `unauthorized`; 429 -> `rate_limited` (message includes `retry_after`); 400/403/404 and other 4xx -> `permanent`; 5xx, 409 and network errors -> `transient`.

## Tests

`python3 -m unittest discover -s plugins/chat-telegram -v` (uses a local fake Bot API; no network).

# voisestats2

Небольшой Discord-бот на Rust для приватного сервера. Он считает голосовую активность участников, держит одну постоянную embed-панель со статистикой, пишет логи по voice-событиям, создаёт временные голосовые комнаты и отвечает через DeepSeek в отдельном AI-канале.

## Что умеет бот

- Считает общее время пользователей в голосовых каналах.
- Держит одну persistent stats panel и редактирует её вместо создания новых сообщений.
- Пишет логи входа, выхода и перехода между голосовыми каналами.
- Создаёт временную личную voice-комнату при входе в специальный create-voice канал.
- Удаляет пустые временные voice-комнаты.
- Отвечает на сообщения в выделенном AI-канале через DeepSeek Chat Completions API.

## Стек

- Rust 2021
- Serenity 0.12
- Tokio
- SQLite через rusqlite
- reqwest для DeepSeek API
- tracing / tracing-subscriber для логов

## Требования к Discord

### Gateway Intents

В Discord Developer Portal для бота должны быть включены:

- `Server Members Intent`
- `Message Content Intent`

В коде используются intents:

- `GUILDS`
- `GUILD_VOICE_STATES`
- `GUILD_MEMBERS`
- `GUILD_MESSAGES`
- `MESSAGE_CONTENT`

### Права бота

Для корректной работы роли бота нужны как минимум:

- `View Channels`
- `Send Messages`
- `Embed Links`
- `Read Message History`
- `Manage Channels`
- `Move Members`
- `Connect`

## Конфигурация

Создайте `.env` на основе `.env.example`.

Основные переменные:

- `DISCORD_TOKEN` — токен Discord-бота
- `GUILD_ID` — ID сервера
- `PANEL_CHANNEL_ID` — канал для панели статистики
- `LOG_CHANNEL_ID` — канал для логов голосовых событий
- `CREATE_VOICE_CHANNEL_ID` — специальный voice-канал для создания временных комнат
- `DATABASE_PATH` — путь к SQLite базе
- `PANEL_UPDATE_SECONDS` — период обновления панели
- `AI_CHANNEL_ID` — канал, в котором бот отвечает через DeepSeek
- `DEEPSEEK_API_KEY` — ключ DeepSeek API
- `DEEPSEEK_BASE_URL` — базовый URL API, по умолчанию `https://api.deepseek.com`
- `DEEPSEEK_MODEL` — модель, по умолчанию `deepseek-v4-flash`
- `AI_MAX_TOKENS` — ограничение на размер ответа модели
- `AI_TIMEOUT_SECONDS` — таймаут запроса к DeepSeek
- `AI_HISTORY_LIMIT` — сколько последних релевантных сообщений брать в контекст
- `AI_COOLDOWN_SECONDS` — простой cooldown на пользователя в AI-канале
- `RUST_LOG` — уровень логирования

Если `DEEPSEEK_API_KEY` не задан, остальная часть бота продолжит работать, а AI-ответы будут мягко завершаться ошибкой без падения процесса.

## Запуск локально

```bash
cargo fmt
cargo check
cargo run
```

## Как работает панель статистики

- Бот хранит `panel_message_id` и `panel_channel_id` в SQLite таблице `settings`.
- При обновлении он сначала пытается отредактировать сохранённое сообщение.
- Если ссылка устарела, бот сканирует историю `PANEL_CHANNEL_ID`, находит свою панель по стабильному маркеру и восстанавливает её.
- Если в канале есть несколько старых панелей, бот оставляет одну активную и пытается удалить подтверждённые дубликаты.
- Новая панель создаётся только если валидной панели нет совсем.

## Как работает AI-канал

- Бот реагирует только на сообщения в `AI_CHANNEL_ID`.
- Игнорируются сообщения других ботов, собственные сообщения бота и пустые сообщения.
- Перед запросом к DeepSeek бот показывает typing indicator.
- В запрос отправляется системный промпт и короткая история последних релевантных сообщений из этого же канала.
- Длинные ответы автоматически режутся на несколько Discord-сообщений.

## Замечания по эксплуатации

- SQLite база и её WAL-файлы создаются автоматически по `DATABASE_PATH`.
- Проект не использует slash commands, кнопки, веб-панель, Redis, PostgreSQL или Docker.
- Для production-пуска можно использовать любой supervisor, например `systemd`, но готовый unit-файл в репозитории не включён.

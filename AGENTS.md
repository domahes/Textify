# Соглашения проекта Textify RS

## Git

- Главная ветка репозитория — **`master`** (не `main`). Все изменения
  коммитятся и отправляются в `master`; отдельную ветку `main` не создавать.
- Удалённый репозиторий: `git@gitverse.ru:domahes/Texhes.git` (`origin`).
  Ветка `master` по умолчанию отслеживает `origin/master`.
- Подпись автора коммитов — аккаунт Gitverse: `Евгений Харченко
  <91953+domahes@users.no-reply.gitverse.ru>` (настроена локально в `.git/config`).
- Не коммитить `target/`, `TextifyRS.ini`, `*.pdb` — они в `.gitignore`.

## Код

- Проект — Rust-приложение только под Windows; интерфейс и сообщения —
  на русском языке.
- Извлечение текста идёт тремя провайдерами (UI Automation, MSAA, `WM_GETTEXT`),
  режим «всё меню» — в `src/menu.rs`.
- Перед коммитом: `cargo build`, `cargo test`; живой тест режима меню —
  `cargo test -- --ignored`.

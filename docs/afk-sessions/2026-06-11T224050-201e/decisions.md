# /afk decisions journal

## 2026-06-11T22:50:03.422Z — Phase-4 звоночек завершения ИИ
- class: `reversible-act`
- context: -
- rationale: Новый модуль M-SIGNAL + интеграция в render/main; контракт чёткий, всё на afk-ветке, обратимо (git reset).
- outcome: src/signal.rs + подсветка bell + hooks/claudebar-bell.ps1; 19 тестов passed, grace lint 0; коммит на afk-ветке.

## 2026-06-11T22:53:31.940Z — Phase-5 иконки приложений
- class: `reversible-act`
- context: -
- rationale: Чистый UI: новый модуль M-ICON + DrawIconEx в секции; Win32 API скомпилировались, обратимо.
- outcome: src/icon.rs (section_icon, кэш) + иконка 16x16 в заголовке; 20 тестов, lint 0; коммит.


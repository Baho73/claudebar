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

## 2026-06-11T22:57:42.310Z — Phase-6 зоны + кнопка закрытия
- class: `reversible-act`
- context: -
- rationale: Введён зональный hit_test и close через PostMessage WM_CLOSE; поправил сигнатуру PostMessageW (HWND, не Option). Обратимо.
- outcome: Zone/hit_test + ✕ на hover + activate::close; 21 тест, lint 0; коммит.

## 2026-06-11T23:02:29.292Z — Phase-7 показать все недавние
- class: `reversible-act`
- context: -
- rationale: Снят лимит 6 в M-RECENT, состояние showall в M-CONFIG (ra=), Row::RecentMore в M-RENDER. Обратимо.
- outcome: 22 теста, lint 0; коммит.


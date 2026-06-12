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

## 2026-06-11T23:14:19.542Z — Phase-8 drag-reorder
- class: `reversible-act`
- context: -
- rationale: Логика порядка/маппинга вынесена в чистые тестируемые функции (config + render helpers); UI drag через SetCapture. Интерактивный drag не проверить в AFK — отложен ручной smoke.
- outcome: ручной порядок секций/окон + режим reorder по ПКМ; 26 тестов, lint 0; коммит.

## 2026-06-11T23:15:11.180Z — План v0.3 исчерпан — все 5 фаз готовы
- class: `checkpoint`
- context: -
- rationale: Phase-4..8 реализованы, по коммиту на фазу на afk-ветке; cargo test 26 passed, grace lint 0, release build exit 0.
- outcome: 8/8 модулей покрыты, 0 pending. 2 отложенных: подключение хука в settings.json, ручной smoke drag.


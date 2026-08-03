# Locale translation status

Source of truth for UI strings: `en-US/app.ftl`.

Locales: `en-US`, `ja-JP`, `th-TH`, `zh-CN`.

## Pipeline

1. Edit English keys in `packages/shared/locales/en-US/app.ftl` (or regenerate via `node scripts/generate-app-ftl.mjs`).
2. Refresh English→locale maps: the committed `scripts/translations/{zh-CN,ja-JP,th-TH}.json` maps English source strings to translations.
3. Apply maps into locale FTL files: `node scripts/apply-locale-translations.mjs`.
4. Native app loads FTL at compile time via `crates/SphereUIComponents/src/i18n.rs`.

## Runtime wiring

`settings.general.language` selects the locale. Surfaces should call `I18n::new(&language)` and `i18n.tr("key")` / `i18n.tr_vars(...)`.

Menu labels use stable IDs from `native-menu.json` mapped as `menu.{id_with_dots_as_dashes}`.

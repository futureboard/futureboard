# APAK tools

APAK packages Futureboard samples, presets, plugins, themes, and services into Ed25519-signed `.apak` archives.

## Create a package

```sh
apak create my-samples --type sample
apak create my-presets --type preset
apak create my-plugin --type plugin
```

Supported `--type` values are `sample`, `preset`, `plugin`, `theme`, `service`, and `extension`. Plural names such as `plugins` are accepted as aliases.

`create` derives the package name from the destination directory and generates an ID such as `local.my-plugin`. Metadata can be supplied on the command line:

```sh
apak create my-plugin --type plugin \
  --id com.example.my-plugin \
  --name "My Plugin" \
  --version 1.0.0 \
  --publisher "Example Audio" \
  --description "My Futureboard plugin" \
  --license MIT
```

The destination must be new or empty. Add package files under `assets`, then build the archive:

```sh
apak pack my-plugin my-plugin.apak
```

`apak init [directory]` remains available for creating the original generic sample template.

## Inspect and install

```sh
apak info my-plugin.apak
apak install my-plugin.apak
apak roots
```

Signed format v2 is the default. `apak pack` loads the private signing key in this order:

1. `--signing-key <file>`
2. the `APAK_SIGNING_KEY` process environment variable
3. `APAK_SIGNING_KEY` in the current directory's `.env`
4. `signed.key` in the current directory

The repository's `signed.key`, `.env`, and `apak.public.key` stay local and are ignored by Git. GitHub Actions stores the production values as `APAK_SIGNING_KEY` and `APAK_VERIFYING_KEY` repository secrets; release builds bake only `APAK_VERIFYING_KEY` into the installer. Users can inspect and install packages without a key because signature verification happens automatically before decompression or installation.

The signed payload is authenticated but not encrypted. Do not put secrets inside an APAK package.

Legacy encrypted v1 packages remain available by passing `--secret-file <file>` to `pack`, `info`, or `install`.

## Install locations

| Type        | Destination                                                                            |
| ----------- | -------------------------------------------------------------------------------------- |
| `sample`    | `Documents/Futureboard Studio/Samples`                                                 |
| `preset`    | `Documents/Futureboard Studio/Presets`                                                 |
| `plugin`    | user config `Futureboard Studio/Extensions/Plugins`                                    |
| `theme`     | user config `Futureboard Studio/Extensions/Themes`                                     |
| `service`   | user config `Futureboard Studio/Extensions/Services`                                   |
| `extension` | mixed legacy package whose first asset directory is `Plugins`, `Themes`, or `Services` |

Legacy manifests using `Extention` or `Extentions` continue to load as `Extensions` packages.

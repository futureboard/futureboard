# APAK Package

Place the files to package inside `assets`, then run
`apak pack <package-directory> <output.apak>`. APAK signs the package with the configured project key; installers verify it automatically.

Package types:

- `Sample`: installs into `Documents/Futureboard Studio/Samples`
- `Preset`: installs into `Documents/Futureboard Studio/Presets`
- `Plugin`: installs into the user `Extensions/Plugins` directory
- `Theme`: installs into the user `Extensions/Themes` directory
- `Service`: installs into the user `Extensions/Services` directory
- `Extensions`: legacy mixed package; assets must start with `Themes`, `Plugins`, or `Services`

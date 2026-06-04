# upload-symbols

upload-symbols is a library and a command-line tool to upload symbols to the [Mozilla Symbols Server][1].

[1]: https://symbols.mozilla.org/

## Installation

Prebuilt binaries for the latest release are available on the [GitHub release page][4].

If you want to install the latest version in an automated workflow, you can directly download the distribution package for your architecture, e.g.
```bash
TARGET="x86_64-unknown-linux-gnu"
curl --tlsv1.2 -fsSLO "https://github.com/mozilla-services/upload-symbols/releases/latest/download/upload-symbols-cli-$TARGET.tar.xz"
tar xaf "upload-symbols-cli-$TARGET.tar.xz"
cd "upload-symbols-cli-$TARGET/"
./upload-symbols --help
```
We will evolve the upload protocol of the Mozilla Symbols Server, and only the latest version of the upload-symbols CLI is guaranteed to be compatible with the current upload API. In automation, we recommend downloading the binary directly before using it. For persistent installations, we recommend checking for upgrades at least on a weekly basis.

[4]: https://github.com/mozilla-services/upload-symbols/releases/latest

## Usage

### Authentication

To use upload-symbols you need an account with upload permissions on the Mozilla Symbols Server. To generate an authentication token, navigate to [the API tokens page][2] and generate a token with either the "Upload Symbols Files" or "Upload Try Symbols Files" permission; the token can only have one of these two permissions. Set the `SYMBOLS_AUTH_TOKEN` environment variable to the secret key of the generated token.
```bash
export SYMBOLS_AUTH_TOKEN="1a2b3c4d..."
```

[2]: https://symbols.mozilla.org/tokens

### Preparing the upload directory

Symbols files are available on the Symbols Server under a path with this structure:
```
<debug_filename>/<debug_id>/<symbol_filename>
```
Before uploading your symbols files, you need to organize them under a single local directory with this path structure. As an example, if your upload directory is `my/upload/directory`, and you want a symbols file to be available at the path `xul.pdb/67060DEB1FD46CFD4C4C44205044422E1/xul.sym`, you need to store it in `my/upload/directory/xul.pdb/67060DEB1FD46CFD4C4C44205044422E1/xul.sym`. The upload directory shouldn't contain any other files. (If it does, they'll be ignored during symbols file discovery, but you'll get an error message in the final summary.)

Symbolic links under the upload directory are permitted and will be followed.

If you are currently building ZIP archives before the upload, you should change your code to generate a directory of symbols files instead. If you only have ZIP archives in the first place, you should unzip these archives to a new directory and use that as the upload directory.

### Uploading

Once you have set `SYMBOLS_AUTH_TOKEN` and prepared the upload directory, you can call
```bash
upload-symbols my/upload/directory
```
to perform the actual upload. The tool sends telemetry to the team maintaining both this CLI and the Mozilla Symbols Server, so we become aware of problems and performance degradations. If the tool crashes, a summary of the crash is sent to [Sentry][3] so we can fix the problem.

You can run `upload-symbols --help` to see the additional command-line flags that are available. For most use cases it's recommended to stick with the default values.

[3]: https://sentry.io/

## License

upload-symbols is dual-licensed under Apache 2.0 and MIT terms.

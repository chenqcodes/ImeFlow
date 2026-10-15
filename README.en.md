<p align="center">
  <img src="assets/icons/imeflow.png" alt="ImeFlow logo" width="128" height="128">
</p>

<h1 align="center">ImeFlow</h1>

<p align="center"><a href="README.md">简体中文</a> | <strong>English</strong></p>

<p align="center"><strong>Let the IME follow your flow.</strong></p>

ImeFlow automatically selects an input mode on Windows based on the foreground application and focused input area. Use Chinese in a chat composer and English in a terminal, code editor, or browser address bar without repeatedly switching modes yourself.

Built with Rust and Windows UI Automation, ImeFlow separates input-area recognition, rule matching, and IME execution. Rules specify a target mode; the application waits for focus to settle and checks the input target again before attempting a switch.

- **Rules for individual input areas:** different parts of the same application can use different modes.
- **Global defaults and point-and-click configuration:** text inputs default to Chinese; terminals, address bars, password fields, and recognized code editors default to English. Override by application, input-area type, or control identifier.
- **Room for manual switching:** each focused area is handled once; stale operations are cancelled when focus changes.
- **System tray controls:** toggle automatic switching, inspect status, edit rules, and open logs.

## Getting started

### Download

The current version is **0.1.0**, for Windows 10/11 x64. Download `ImeFlow-0.1.0-windows-x64.zip` from [GitHub Releases](https://github.com/chenqcodes/ImeFlow/releases/latest), extract the entire archive, and run `imeflow.exe`. ImeFlow runs in the system tray. The prebuilt package does not require Rust or PowerShell 7.

The application UI is currently in Chinese. The language links above switch the project documentation, not the application UI. The detailed [rules guide](docs/rules-guide.md) is also currently in Chinese.

Before upgrading, exit the previous instance from its tray menu and back up your `rules.toml`. Preserve your own rules when replacing files manually. WeChat IME users should read the compatibility section below before enabling automatic switching.

### Build from source

Use Windows 10/11, PowerShell 7 (`pwsh.exe`), and a Rust toolchain. This version was verified with Rust 1.98.1.

| Entry point | Purpose |
| --- | --- |
| `01-build-and-run.cmd` | Test, build, validate PNG assets, then run |
| `02-run-current.cmd` | Run the newest verified build with a matching version stamp |
| `03-open-logs.cmd` | Open the log directory |
| `04-export-latest-log.cmd` | Export key entries from the latest application session |
| `05-verify-only.cmd` | Test, build, and validate assets without starting automation |
| `06-release-package.cmd` | Build the release directory at `dist/ImeFlow` |
| `07-install-local.cmd` | Install into LocalAppData and create a Start menu shortcut |

Only one normal application instance may run at a time. Exit the old instance through its tray menu before starting a replacement. Build scripts do not terminate running instances. To verify code in a separate output directory:

```powershell
./scripts/build-and-run.ps1 -TargetDir target/review-fixes -Lint
./scripts/verify-tooling.ps1 -Exe ./target/review-fixes/debug/imeflow.exe
```

The current-build launcher checks `target/debug`, `target/release`, and the debug/release directories under `target/review-fixes` and `target/rule-settings`. Both the executable's `--version` output and build stamps use `build-id.txt`.

## Rules and switching

ImeFlow loads `rules.toml` at startup. After a focus change, it matches the process name and recognized input area to a **target mode**. The switching policy and IME backend then decide whether and how to act. A matched target does not mean the actual input mode has already changed.

Use **通用默认设置** (global defaults) in the tray menu to adjust defaults for all applications. Use **点选输入区域设置** (pick an input area) to select a field and add an override. The panel shows the recognized area, matching scope, and source of the current rule. If no unique control identifier is available, only application or area-level overrides are offered.

Saving validates and backs up the configuration, detects intervening changes, and reloads the rules. Re-enter the input area to apply them. Saving does not enable automatic switching if you have turned it off. You can also use **打开规则配置** (open rules) and **重新加载规则** (reload rules); a failed reload retains the last valid rules.

**规则管理 / 导入导出** (rule management / import and export) supports filtering, editing, deleting overrides, previewing application presets, importing, exporting, and restoring the previous configuration. See the [rules guide (Chinese)](docs/rules-guide.md) for examples and limitations.

### Configuration location

The application first looks for `rules.toml` **beside `imeflow.exe`**, then falls back to the working directory. The tray command opens the file actually used by that instance.

- During development, the repository's `rules.toml` is a template for new builds. Build/start scripts copy it only when the selected output directory has no configuration.
- For packaged or installed builds, edit the configuration beside the executable. The local installer preserves existing rules during upgrades. Manual archive extraction requires you to preserve your own configuration.

Changing the repository template does not update an existing instance's rules. Panel saves reload automatically and maintain a `rules.toml.bak` backup; manual file edits require a tray reload.

### Example

These rules use Chinese in Cursor text inputs and the IME's English mode in its integrated terminal:

```toml
[[rules]]
process = "Cursor.exe"
control = "edit"
mode = "native-chinese"

[[rules]]
process = "Cursor.exe"
control = "terminal"
mode = "ime-english"
```

Valid `control` values are `terminal`, `address-bar`, `edit`, `password`, `code-editor`, and `other`. Omit it for an application-wide default. New global defaults already cover the text-input and terminal behavior above; add application rules for exceptions.

| Mode | Meaning |
| --- | --- |
| `native-chinese` | Chinese mode of a Chinese IME |
| `ime-english` | English mode within the same IME |
| `layout-english` | US English keyboard layout (ENG) |
| `passthrough` | Leave the input mode alone |

Process names match exactly, ignoring case. Precedence is **control identifier > application area > application default > global default > passthrough**. Global defaults also apply to unknown applications. Older files without global terminal/password defaults retain their previous passthrough fallback for those areas. Invalid fields, modes, and duplicate rules cause loading to fail with an explanation.

### Execution and status

The pipeline handles window/focus events, debouncing, UIA recognition, rule matching, stability checks, policy decisions, guarded IME execution, and status/statistics updates. Recognizable Monaco code structure takes precedence over filename keywords; composer/prompt structure helps distinguish chat inputs.

Transactions retain the original HWND and UIA RuntimeId, checking focus after reads and waits and before input or writes. Focus changes cancel stale transactions. Failures known to have no side effects may be retried within a bounded budget. Failures after a key was sent or a write may have occurred are not automatically retried, avoiding a second Shift that reverses the first.

For WeChat IME, the backend combines IMM open and conversion readings. A closed context is treated as probably English even when the conversion flags retain the Chinese bit. Raw readings must remain stable for at least 240 ms within the same focus event, followed by two checks 120 ms apart immediately before execution. Matching targets are skipped; unknown or changing readings are deferred. A single guarded Shift is sent only when the observed state is opposite to the target, for either direction. This path does not combine IMM writes with a Shift fallback or rely on remembered target modes.

These delays filter observed focus-restoration transitions; they cannot cover every IME version or synchronization delay. Stable IMM readings can still differ from actual typing behavior, so WeChat status remains **inferred / unconfirmed**. A skipped request is not counted as a switch and does not activate post-Shift handling.

A separate read-only tray observer schedules a background read every 400 ms and updates the icon after two stable observations. It does not send keys, replay rules, edit configuration, or increment automatic-switch counts. Stale readings are discarded when focus or execution changes. Tooltips distinguish confirmed, inferred, and unknown states. Pending or unconfirmed requests without a current observation use a neutral icon rather than displaying the requested mode as achieved. Navigation controls such as buttons and sidebar items do not trigger switching.

The keyboard hook is managed serially on a dedicated thread, installed on demand, and removed when idle. UIA interfaces and property-cache requests are reused per thread, with fresh snapshots for each focus check.

## IME settings and compatibility

An IME's default mode, per-application memory, global synchronization, and Chinese/English shortcut can affect ImeFlow. Correct input-area recognition does not guarantee successful switching. Brief icon flickering after a focus change does not establish that the final typing mode is wrong. Keep IME settings fixed while testing.

| IME / setting | Requirements and current limitations |
| --- | --- |
| WeChat IME | Enable **默认输入状态 → 切换中英文对所有应用生效** (apply Chinese/English switching to all applications), and retain standalone Shift for switching. Labels may vary by version. ImeFlow does not read or modify this setting automatically. Global synchronization disabled or a changed Shift shortcut has not been verified. |
| First cross-application focus with WeChat IME | Brief Chinese/English indicator flickering was observed locally even with ImeFlow automation disabled. Context restoration or synchronization may explain it, but the internal mechanism is unconfirmed. Check actual typing. |
| Microsoft Pinyin | The backend currently treats IMM readings as trusted and retains its existing execution path. The terminal English path uses Shift. Recheck behavior after changing IME versions, compatibility mode, or per-application settings. |
| Other Chinese IMEs | A shared IMM backend exists, but each IME has not been verified through real typing. This does not establish compatibility with Sogou, Weasel, or other products. Disable automation or use `passthrough` if readings, shortcuts, or focus restoration behave unexpectedly. |
| Windows per-app input method setting | “Let me use a different input method for each app window” is separate from WeChat's global Chinese/English mode setting. Check keyboard/IME selection separately from the mode inside the IME; there is no blanket requirement to change this Windows option. |

For initial setup or after an IME upgrade:

1. Disable ImeFlow automation. Record IME version, global/per-app settings, and shortcuts; observe native behavior between applications and between input areas.
2. For WeChat IME, enable the global setting above and keep Shift switching. Let the state settle before enabling ImeFlow.
3. Move repeatedly between a terminal, web text field, address bar, and chat input. Returning to a terminal should preserve English; Chinese fields should accept Chinese. Do not execute test text in the terminal.
4. Switch manually within one area and verify that it is not immediately reversed. Also switch manually in another application and return. Check first focus and rapid transitions.
5. For issues, provide Windows, IME, and application versions, relevant settings, steps, and debug logs for the affected time. Review logs before sharing. Neither log readings nor tray icons replace actual typing checks.

Automated regressions cover stale Chinese conversion bits with a closed context, repeated terminal/address-bar returns, manual changes across applications, stability windows, and backend changes before sending keys. **They do not establish real-typing compatibility across every IME, version, and settings combination.**

## Logs and diagnostics

Logs default to `%LOCALAPPDATA%/ImeFlow/logs`; `IMEFLOW_LOG_DIR` overrides the location for the application, tray, and scripts.

The default level is `warn`, recording warnings, errors, and a per-session logging header. For development, copy `.env.example` to `.env` in the project root:

```dotenv
IMEFLOW_LOG_LEVEL=debug
```

Supported levels: `off`, `error`, `warn`, `info`, `debug`, and `trace`. Restart after changes. Precedence is `RUST_LOG`, the `IMEFLOW_LOG_LEVEL` environment variable, `--debug`, `.env`, then `warn`. `RUST_LOG` also supports module filters such as `imeflow=debug`.

The application first checks for `.env` beside the executable. Debug builds also check the development root; release builds do not read the development or working directory's `.env`. Packaging and installation do not copy `.env`. Only `IMEFLOW_LOG_LEVEL` is read from this file; variables are not expanded.

Debug logs describe focus events, classification, targets, observation confidence, mode memory, skipped operations, and transactions. Normal logging does not record UIA control names, chat content, ancestor text, or typed letters. Debug logging alone does not install extra keyboard hooks.

- `session-*.log`: retained application-session logs.
- `latest.log`: current session entry, updated only after acquiring single-instance ownership.
- `diagnostic-*.log`: separate asset-check/probe logs.
- `latest-key.log`: key entries exported by script 04.

`--version` does not initialize logs. `--check-assets` does not install input hooks, change the IME, or write statistics. `--probe-focus` and `--probe-ime` are read diagnostics; TSF global compartments do not prove a target control's actual mode. `--probe-shift`, `--verify-switch`, and `--trace-probe` perform input or switching and should only be used for intentional testing.

## Verification

```powershell
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

Tests cover classification, rules, focus checks, switching policy, side effects and retry boundaries, log isolation, single-instance behavior, and Windows hook-thread lifecycle. Automated checks do not replace real typing checks with Microsoft Pinyin or WeChat IME in your applications.

## License and privacy

Project code and project-drawn icons use the [MIT license](LICENSE). Third-party Rust dependencies retain their own licenses; release packages include their license texts.

ImeFlow inspects the foreground application and UI Automation controls locally to choose an input mode. Rules, statistics, and diagnostic logs are stored locally. Normal logs do not record typed characters or chat text. Explicit diagnostic commands may output control names and input-area information; review them before sharing.

Secrets, `.env`, logs, rule backups, local notes, editor records, and build outputs are excluded from source control. Provide a minimal reproduction when reporting issues rather than uploading a complete personal configuration or log directory.

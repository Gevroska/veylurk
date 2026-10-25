# Windows operation, startup and recovery

Extract the complete Windows package into a stable, writable folder owned by your Windows user, for example `C:\Apps\Veylurk`. Keep every `.ps1` file beside `veylurk.exe`; keep its `data` directory with the package. Use the same Windows account for collection and remembered Twitch credentials. The scripts require Windows PowerShell 5.1 and do not change execution policy. Reparse points in operation and backup paths are refused.

The usual foreground launcher remains available:

```powershell
powershell.exe -NoProfile -File .\Start-Veylurk.ps1
```

Close the local web page whenever desired; collection continues. Ctrl+C requests a graceful foreground stop. Check the local status page for storage errors and channel coverage. A successful process start does not prove that Twitch supplies useful membership data.

## Optional background operation

To run without a console window, from the extracted package:

```powershell
Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoProfile -NonInteractive -WindowStyle Hidden -File "C:\Apps\Veylurk\Start-VeylurkBackground.ps1"'
```

Replace the example path with your package's absolute path. Add `-Port 9123` inside the argument string to use another port. The supervisor starts the exact executable beside its script with an absolute `data\observations.sqlite3` path, then waits for it. It drains and discards application output and records only UTC start/stop events and numeric exit codes in `data\background.log`. The log rotates after approximately 256 KiB, retaining one previous file. It contains no application output, credentials or request URLs. The application separately writes a bounded fault journal beside the database; see [local diagnostics](diagnostics.md). A hidden launch error might produce no visible window; inspect those operational records or use the foreground launcher to diagnose configuration or credential problems.

Stop the selected package gracefully:

```powershell
powershell.exe -NoProfile -File .\Stop-Veylurk.ps1
```

Use the same `-Port` if changed. The stop script checks the runtime's database and executable paths, process ID, loopback listener ownership and instance identity. It retains the selected process handle, sends a same-origin JSON shutdown request bound to that instance, waits up to 60 seconds (`-TimeoutSeconds` changes the wait), and requires a successful exit code. It never terminates arbitrary processes. If shutdown is still pending, wait and investigate; do not force termination to obtain a backup. A nonzero exit can indicate that SQLite could not finish its checkpoint, for example because another database reader held it open. The process has ended, but this is not a confirmed clean shutdown: inspect the retained database and WAL, close external readers, then reopen and gracefully stop that same history before backing it up. Do not delete its WAL. The application must finish closing the collector and SQLite worker before an offline backup can succeed.

## Optional startup at login

Login startup is explicitly opt-in. Installation only registers the task; it does not start the application:

```powershell
powershell.exe -NoProfile -File .\Install-VeylurkStartup.ps1
```

The script prints the task name. The name is derived from the absolute package location and current Windows user. The task uses that user's interactive login token at limited privilege, with no password or administrator requirement under normal Windows policy. Organizational Task Scheduler policy can still prevent registration. It starts at that user's next login, uses hidden windows and a stable working directory, ignores overlapping starts, and has no execution time limit. Nonzero supervisor exits trigger up to three restarts, one minute apart. A normal shutdown exits successfully and does not trigger failure retries. This task does not run before login and collection does not survive sign-out or shutdown.

For immediate startup after installation, use Task Scheduler's **Run** action for the printed task name. The background supervisor by itself does not restart crashed processes; the task supplies bounded failure retries. The task does not recover a permanently invalid configuration or unavailable disk. There is no unattended credential prompt: remember optional credentials through `Setup-Twitch.ps1` beforehand, or run chat-only collection. Current-user DPAPI works because the task runs in the same user session. Credentials are never stored in task arguments or task configuration.

To remove future starts and pending failure retries:

```powershell
powershell.exe -NoProfile -File .\Remove-VeylurkStartup.ps1
powershell.exe -NoProfile -File .\Stop-Veylurk.ps1
```

Removal verifies that the task action belongs to this package and removes its registration. It does not force-stop the application or delete history. Use graceful stop for the running process. Task Scheduler's **End** action and closing the supervisor forcibly are not substitutes for a graceful stop. When upgrading or moving the package, remove its old startup task first, stop the old executable, retain its entire data directory, extract the new package, and explicitly reinstall startup from the new location. A newly downloaded executable can require a fresh Windows firewall decision before its first Internet access.

## Offline backup

Remove login startup first if installed, then stop gracefully. This also avoids a scheduled retry racing maintenance. Choose a new backup directory whose parent already exists:

```powershell
powershell.exe -NoProfile -File .\Backup-Veylurk.ps1 -Destination 'D:\Backups\Veylurk-2026-10-25'
```

The script acquires exclusive access to `data\observations.sqlite3.lock`, compatible with the application's Windows file lock. It keeps that lock throughout copying and verification, preventing another Veylurk storage worker from opening this database. A running database or nonempty SQLite WAL makes the operation fail safely. If an interrupted process left a nonempty WAL, reopen that same database with Veylurk and stop it gracefully before retrying; do not delete the WAL.

The backup contains the main database (including channel configuration, history, metadata and application settings), remembered `twitch-credentials.dpapi` if present, and a versioned SHA-256 manifest. It does not include lock files, logs, plaintext credentials or executable files. Credential ciphertext is copied unchanged and can normally be decrypted only by the original Windows user on the original Windows installation. Re-enter credentials on another installation rather than expecting portable decryption. Hashes detect accidental corruption; they do not authenticate a backup against malicious modification. Protect backups as personal history and retain separate copies on reliable storage.

Each destination must be new. The script verifies a temporary sibling directory before moving it into place. A failure can leave a `.veylurk-backup-*` directory for inspection; an existing backup is never replaced. Backup scripts support the standard packaged database path only; direct executable launches with custom database paths need a separate, explicitly scoped maintenance procedure.

## Restore without replacing history

Extract a fresh package into a new directory. Keep it stopped and do not install startup until restoration is finished. Its `data` directory must not yet exist:

```powershell
powershell.exe -NoProfile -File .\Restore-Veylurk.ps1 -BackupDirectory 'D:\Backups\Veylurk-2026-10-25' -DataDirectory 'C:\Apps\Veylurk-Restored\data'
```

Restore checks the manifest, allowed filenames, lengths and every SHA-256 hash. It copies into a temporary sibling, verifies the copies again, and moves the directory into the fresh target. Existing directories are rejected, including empty ones. The backup and prior application's data remain intact. Failed restores can leave a `.veylurk-restore-*` directory for inspection. The scripts never decrypt credentials during backup or restore.

Start the restored package normally, check saved channels and settings, search known historical accounts, and confirm optional credentials work for this Windows user. Remove the old task before installing startup for the restored package. Keep the old history until those checks pass. Manifest checks prove byte preservation; only opening the restored database and checking the expected records proves application-level recovery.

## Validation boundaries

`Test-VeylurkOperations.ps1` uses isolated invented files, real DPAPI ciphertext and file locks, plus mocked Task Scheduler and HTTP operations. It never starts Veylurk, contacts Twitch or changes actual startup tasks. CI additionally starts the packaged application without channels or credentials, checks active-lock refusal, calls the real graceful shutdown endpoint, and verifies backup/restore with its actual SQLite file. Neither fixture demonstrates a long-running live collector or a real login/reboot cycle. These remain explicit acceptance checks on the intended Windows account and machine.

# Oracle Instant Client

Dipper's Oracle build loads Instant Client from the process `PATH`. It does not store a database address, account, or password in this document. Set those only as the environment variables `DIPPER_ORA_USER` and `DIPPER_ORA_PASSWORD` in your own config.

## Windows

1. Download Oracle Instant Client Basic, or Basic Light, for 64-bit Windows from Oracle.
2. Unzip it into a folder you control, for example under your user profile.
3. Install the Microsoft Visual C++ redistributable that the Instant Client package requires.
4. Add that unzipped folder to your user `PATH` only. This reads and writes the user PATH. It does not read or change the machine PATH. In PowerShell:

```powershell
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ([string]::IsNullOrEmpty($userPath)) {
    $userPath = "C:\path\to\instantclient"
} else {
    $userPath = "$userPath;C:\path\to\instantclient"
}
[Environment]::SetEnvironmentVariable("Path", $userPath, "User")
```

Replace `C:\path\to\instantclient` with the folder that contains the Instant Client libraries. Open a new terminal so the updated user `PATH` is visible.

5. Build or install a Dipper binary with the `oracle` feature.
6. Run `dipper check --env-only`. The `instant_client` item is `ok` when the library loads, and `fail` with `DPI-1047` when it does not. The command does not open a database.

## What Dipper checks

`dipper check` prints the environment before it opens any source. `--env-only` stops after that section. The report names environment variables and never prints their values.

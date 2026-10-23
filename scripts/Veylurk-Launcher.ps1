# Shared Windows PowerShell 5.1 launcher functions. This file does not launch anything.
Set-StrictMode -Version 2.0

function ConvertTo-VeylurkArgument {
    param([AllowEmptyString()][string]$Value)
    $result = New-Object System.Text.StringBuilder
    [void]$result.Append('"')
    $slashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') { $slashes++; continue }
        if ($character -eq '"') {
            [void]$result.Append(('\' * ($slashes * 2 + 1)))
        } else {
            [void]$result.Append(('\' * $slashes))
        }
        [void]$result.Append($character)
        $slashes = 0
    }
    [void]$result.Append(('\' * ($slashes * 2)))
    [void]$result.Append('"')
    $result.ToString()
}

function Assert-VeylurkCredentials {
    param($Credentials)
    foreach ($value in @($Credentials.ClientId, $Credentials.ClientSecret)) {
        if ([string]::IsNullOrEmpty($value) -or $value.Length -gt 512 -or $value -match '\p{Cc}') {
            throw 'Enter both a valid application client ID and client secret.'
        }
    }
}

function Get-VeylurkCredentialPath {
    param([string]$PackageDirectory)
    Join-Path $PackageDirectory 'data\twitch-credentials.dpapi'
}

function Read-VeylurkCredentials {
    param([string]$PackageDirectory)
    $id = [Environment]::GetEnvironmentVariable('VEYLURK_TWITCH_CLIENT_ID', 'Process')
    $secret = [Environment]::GetEnvironmentVariable('VEYLURK_TWITCH_CLIENT_SECRET', 'Process')
    if ($null -ne $id -or $null -ne $secret) {
        $credentials = [pscustomobject]@{ ClientId = $id; ClientSecret = $secret }
        Assert-VeylurkCredentials $credentials
        return $credentials
    }
    $path = Get-VeylurkCredentialPath $PackageDirectory
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    $plain = $null
    try {
        Add-Type -AssemblyName System.Security
        $encrypted = [IO.File]::ReadAllBytes($path)
        $plain = [Security.Cryptography.ProtectedData]::Unprotect($encrypted, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
        $credentials = [Text.Encoding]::UTF8.GetString($plain) | ConvertFrom-Json
        Assert-VeylurkCredentials $credentials
        return $credentials
    } catch {
        throw 'Saved Twitch credentials could not be opened for this Windows user. Run Setup-Twitch.ps1 to replace them.'
    } finally {
        if ($null -ne $plain) { [Array]::Clear($plain, 0, $plain.Length) }
    }
}

function Save-VeylurkCredentials {
    param([string]$PackageDirectory, $Credentials)
    Assert-VeylurkCredentials $Credentials
    $plain = $null
    $temporary = $null
    try {
        Add-Type -AssemblyName System.Security
        $path = Get-VeylurkCredentialPath $PackageDirectory
        $directory = Split-Path -Parent $path
        [void][IO.Directory]::CreateDirectory($directory)
        $plain = [Text.Encoding]::UTF8.GetBytes(($Credentials | ConvertTo-Json -Compress))
        $encrypted = [Security.Cryptography.ProtectedData]::Protect($plain, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
        $temporary = Join-Path $directory ('credentials-' + [guid]::NewGuid().ToString('N') + '.tmp')
        [IO.File]::WriteAllBytes($temporary, $encrypted)
        if (Test-Path -LiteralPath $path) { [IO.File]::Replace($temporary, $path, [System.Management.Automation.Language.NullString]::Value) }
        else { [IO.File]::Move($temporary, $path) }
    } catch {
        throw 'Twitch credentials could not be saved. Check write access to the package data folder.'
    } finally {
        if ($null -ne $plain) { [Array]::Clear($plain, 0, $plain.Length) }
        if ($temporary -and (Test-Path -LiteralPath $temporary)) { Remove-Item -LiteralPath $temporary -ErrorAction SilentlyContinue }
    }
}

function Assert-VeylurkReady {
    param([string]$PackageDirectory, [int]$Port)
    if (-not (Test-Path -LiteralPath (Join-Path $PackageDirectory 'veylurk.exe') -PathType Leaf)) {
        throw 'Keep the launcher scripts beside the extracted veylurk.exe.'
    }
    $reservation = New-Object Net.Sockets.TcpListener([Net.IPAddress]::Loopback, $Port)
    try {
        $reservation.Server.ExclusiveAddressUse = $true
        $reservation.Start()
    } catch {
        throw "Local port $Port is already in use or unavailable. Stop the existing app with Ctrl+C, or choose another -Port."
    } finally { $reservation.Stop() }
}

function New-VeylurkProcessInfo {
    param([string]$Executable, [string]$WorkingDirectory, [string[]]$Arguments, $Credentials)
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $Executable
    $info.WorkingDirectory = $WorkingDirectory
    $info.Arguments = (($Arguments | ForEach-Object { ConvertTo-VeylurkArgument $_ }) -join ' ')
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $false
    $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_ID')
    $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_SECRET')
    if ($null -ne $Credentials) {
        Assert-VeylurkCredentials $Credentials
        $info.EnvironmentVariables['VEYLURK_TWITCH_CLIENT_ID'] = $Credentials.ClientId
        $info.EnvironmentVariables['VEYLURK_TWITCH_CLIENT_SECRET'] = $Credentials.ClientSecret
    }
    return $info
}

function Start-VeylurkApplication {
    param([string]$PackageDirectory, [int]$Port, $Credentials)
    Assert-VeylurkReady $PackageDirectory $Port
    $database = Join-Path $PackageDirectory 'data\observations.sqlite3'
    $info = New-VeylurkProcessInfo (Join-Path $PackageDirectory 'veylurk.exe') $PackageDirectory @('--database', $database, '--port', [string]$Port) $Credentials
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    try {
        [Console]::WriteLine("Starting Veylurk at http://127.0.0.1:$Port . Keep this console open; press Ctrl+C to stop gracefully.")
        if (-not $process.Start()) { throw 'Application start failed.' }
        $process.WaitForExit()
        return $process.ExitCode
    } catch {
        throw 'Veylurk could not start or complete normally. Check the application console and package files.'
    } finally {
        $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_ID')
        $info.EnvironmentVariables.Remove('VEYLURK_TWITCH_CLIENT_SECRET')
        $process.Dispose()
    }
}

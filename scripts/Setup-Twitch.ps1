#requires -Version 5.1
[CmdletBinding()]
param([ValidateRange(1, 65535)][int]$Port = 8794)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Veylurk-Launcher.ps1')
$credentials = $null
$form = $null
try {
    Assert-VeylurkReady $PSScriptRoot $Port
    if ([Threading.Thread]::CurrentThread.ApartmentState -ne 'STA') { throw 'Run this setup using powershell.exe -NoProfile -STA -File .\Setup-Twitch.ps1' }
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    [Windows.Forms.Application]::EnableVisualStyles()
    $form = New-Object Windows.Forms.Form
    $form.Text = 'Veylurk - Twitch application setup'
    $form.ClientSize = New-Object Drawing.Size(560, 330)
    $form.StartPosition = 'CenterScreen'
    $form.FormBorderStyle = 'FixedDialog'
    $form.MaximizeBox = $false
    $form.MinimizeBox = $false
    $form.AutoScaleMode = 'Font'
    $intro = New-Object Windows.Forms.Label
    $intro.Text = 'Enter your own Twitch application credentials locally. The secret is masked and is never written to console output.'
    $intro.SetBounds(20, 18, 520, 40)
    $idLabel = New-Object Windows.Forms.Label
    $idLabel.Text = 'Application client ID'
    $idLabel.SetBounds(20, 70, 520, 20)
    $idBox = New-Object Windows.Forms.TextBox
    $idBox.SetBounds(20, 92, 520, 24)
    $idBox.MaxLength = 512
    $secretLabel = New-Object Windows.Forms.Label
    $secretLabel.Text = 'Application client secret'
    $secretLabel.SetBounds(20, 130, 520, 20)
    $secretBox = New-Object Windows.Forms.TextBox
    $secretBox.SetBounds(20, 152, 520, 24)
    $secretBox.MaxLength = 512
    $secretBox.UseSystemPasswordChar = $true
    $remember = New-Object Windows.Forms.CheckBox
    $remember.Text = 'Remember for this Windows user (replaces saved credentials)'
    $remember.SetBounds(20, 195, 520, 24)
    $remember.Checked = $false
    $note = New-Object Windows.Forms.Label
    $note.Text = if (Test-Path -LiteralPath (Get-VeylurkCredentialPath $PSScriptRoot)) { 'Unchecked uses these credentials only for this launch. Previously saved credentials remain for future launches.' } else { 'Unchecked uses credentials only for this launch. Remember stores an encrypted file protected by your Windows account.' }
    $note.SetBounds(20, 228, 520, 40)
    $launch = New-Object Windows.Forms.Button
    $launch.Text = 'Start Veylurk'
    $launch.SetBounds(290, 282, 130, 30)
    $cancel = New-Object Windows.Forms.Button
    $cancel.Text = 'Cancel'
    $cancel.SetBounds(430, 282, 110, 30)
    $cancel.DialogResult = 'Cancel'
    $launch.Add_Click({
        try {
            Assert-VeylurkCredentials ([pscustomobject]@{ ClientId = $idBox.Text.Trim(); ClientSecret = $secretBox.Text.Trim() })
            $form.DialogResult = 'OK'
        } catch {
            [void][Windows.Forms.MessageBox]::Show($form, 'Enter both a valid client ID and client secret.', 'Veylurk', 'OK', 'Warning')
        }
    })
    $form.Controls.AddRange(@($intro, $idLabel, $idBox, $secretLabel, $secretBox, $remember, $note, $launch, $cancel))
    $form.AcceptButton = $launch
    $form.CancelButton = $cancel
    if ($form.ShowDialog() -ne 'OK') { exit 0 }
    $credentials = [pscustomobject]@{ ClientId = $idBox.Text.Trim(); ClientSecret = $secretBox.Text.Trim() }
    $shouldRemember = $remember.Checked
    $idBox.Clear()
    $secretBox.Clear()
    $form.Dispose()
    $form = $null
    Assert-VeylurkReady $PSScriptRoot $Port
    if ($shouldRemember) { Save-VeylurkCredentials $PSScriptRoot $credentials }
    $code = Start-VeylurkApplication $PSScriptRoot $Port $credentials
    exit $code
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    if ('Windows.Forms.MessageBox' -as [type]) { [void][Windows.Forms.MessageBox]::Show($_.Exception.Message, 'Veylurk setup', 'OK', 'Error') }
    exit 1
} finally {
    if ($null -ne $form) { $form.Dispose() }
    $credentials = $null
}

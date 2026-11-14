# Downloads Intel's PresentMon (MIT), pinned to a release and its hash, into target\windows, where
# nativedesktop.toml and install-windows.ps1 pick it up. Run from the checkout.
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)

$presentmonVersion = "2.6.0"
$presentmonSha256 = "B2A706BC6AD475749E3B7E3409263AA1E6906D45BDCF993F6DBC0F660188F1AF"

$presentmon = "target\windows\PresentMon.exe"
if (-not (Test-Path $presentmon) -or (Get-FileHash $presentmon).Hash -ne $presentmonSha256) {
    New-Item -ItemType Directory -Force (Split-Path $presentmon) | Out-Null
    $url = "https://github.com/GameTechDev/PresentMon/releases/download/v$presentmonVersion/PresentMon-$presentmonVersion-x64.exe"
    Invoke-WebRequest -UseBasicParsing $url -OutFile $presentmon
    if ((Get-FileHash $presentmon).Hash -ne $presentmonSha256) { throw "PresentMon download does not match its hash" }
}

# Builds FormalPacer and installs it for this user: the app, the Vulkan layer, the Direct3D and
# OpenGL hook and PresentMon in %LOCALAPPDATA%\Programs\FormalPacer, the layer registered for this
# user, a Start menu entry, and the tray item at sign-in. Run from the checkout: powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)

# Intel's PresentMon (MIT), pinned to a release and its hash.
$presentmonVersion = "2.6.0"
$presentmonSha256 = "B2A706BC6AD475749E3B7E3409263AA1E6906D45BDCF993F6DBC0F660188F1AF"

$env:RUSTUP_TOOLCHAIN = "1.95.0-x86_64-pc-windows-msvc"
cargo build --release -p formalpacer -p formalpacer-layer -p formalpacer-inject
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$presentmon = "target\windows\PresentMon.exe"
if (-not (Test-Path $presentmon) -or (Get-FileHash $presentmon).Hash -ne $presentmonSha256) {
    New-Item -ItemType Directory -Force (Split-Path $presentmon) | Out-Null
    $url = "https://github.com/GameTechDev/PresentMon/releases/download/v$presentmonVersion/PresentMon-$presentmonVersion-x64.exe"
    Invoke-WebRequest -UseBasicParsing $url -OutFile $presentmon
    if ((Get-FileHash $presentmon).Hash -ne $presentmonSha256) { throw "PresentMon download does not match its hash" }
}

$dir = Join-Path $env:LOCALAPPDATA "Programs\FormalPacer"
New-Item -ItemType Directory -Force $dir | Out-Null
Get-Process formalpacer -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq "$dir\formalpacer.exe" } | Stop-Process
Copy-Item target\release\formalpacer.exe, target\release\formalpacer_layer.dll, target\release\formalpacer_inject.dll, $presentmon $dir -Force

$manifestDir = Join-Path $env:LOCALAPPDATA "FormalPacer"
New-Item -ItemType Directory -Force $manifestDir | Out-Null
$manifest = Join-Path $manifestDir "formalpacer.json"
(Get-Content packaging\linux\formalpacer-layer.json.in -Raw).
    Replace("@LAYER_NAME@", "VK_LAYER_FORMALSTUDIO_formalpacer").
    Replace("@LIBRARY_PATH@", "$dir\formalpacer_layer.dll".Replace("\", "\\")).
    Replace("@LIBRARY_ARCH@", "64") | Set-Content $manifest -Encoding ascii
$layers = "HKCU:\SOFTWARE\Khronos\Vulkan\ImplicitLayers"
New-Item -Force $layers -ErrorAction SilentlyContinue | Out-Null
New-ItemProperty -Path $layers -Name $manifest -PropertyType DWord -Value 0 -Force | Out-Null

$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut(
    (Join-Path ([Environment]::GetFolderPath("Programs")) "FormalPacer.lnk"))
$shortcut.TargetPath = "$dir\formalpacer.exe"
$shortcut.WorkingDirectory = $dir
$shortcut.Description = "Frame pacing for games"
$shortcut.Save()

New-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name FormalPacer `
    -Value "`"$dir\formalpacer.exe`" --background" -PropertyType String -Force | Out-Null

Write-Output "Installed in $dir. It starts at sign-in; start it now with: & `"$dir\formalpacer.exe`" --background"

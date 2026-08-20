# Product screenshots, straight from the real UI. S4.8 / screenshots.md.
#
#   powershell -File tray/ui-dev/shoot.ps1 [-Out <dir>]
#
# For each built screen and both themes this renders the app's actual
# ui/ files -- through the ui-dev harness with the standard demo dataset and
# a frozen clock -- at the mockup's reference size 1536x1024, device scale 2,
# so every file is 3072x2048 (the "@2x" the web and the Store want).
#
# All four screens ship: first-setup, queue, review, settings.
#
# Two shell traps this file already paid for, do not refactor them away:
#   - Edge's command-line URL fixup splits URLs at "=" beyond the first
#     parameter, so every harness URL carries exactly ONE query parameter
#     (?shot=<screen>-<theme>); the harness unpacks everything from it.
#   - each Edge run gets its own --user-data-dir; headless runs sharing a
#     profile silently attach to the first process and take no screenshot.
#
# If acme-frame.html's layout changes, re-measure the demo detection boxes:
#   msedge --headless=new --dump-dom http://localhost:4173/tray/ui-dev/fixtures/measure-keys.html
#   msedge --headless=new --dump-dom .../measure-team.html   (via cmd, see above)
# and copy the RECT values into demo.js.

param(
    [string]$Out = "$PSScriptRoot\shots"
)

$ErrorActionPreference = 'Stop'

# The size check below reads each PNG with System.Drawing, which Windows
# PowerShell only has loaded if something else in the session happened to load
# it. Without this the script dies on the FIRST file it tries to measure --
# after writing it -- and leaves the folder half old and half new, which reads
# as a fresh set. Found 19/08 on a clean session.
Add-Type -AssemblyName System.Drawing

$repo = (Resolve-Path "$PSScriptRoot\..\..").Path
$edge = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
if (-not (Test-Path $edge)) { $edge = "C:\Program Files\Microsoft\Edge\Application\msedge.exe" }
if (-not (Test-Path $edge)) { Write-Error "Microsoft Edge not found; the shots ride its headless mode."; exit 1 }

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$profiles = Join-Path $env:TEMP "framekeep-shoot-profiles"

$port = 4173
# serve.py, not `python -m http.server`. Each run gets a fresh Edge profile so
# nothing is cached today -- but the plain server was replaced everywhere else
# for serving stale files, and the script that produces the Store's screenshots
# is the last place worth leaving on the version with that hazard.
$server = Start-Process python -ArgumentList "$PSScriptRoot\serve.py", "$port" `
    -WorkingDirectory $repo -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2

try {
    foreach ($screen in @("first-setup", "queue", "review", "settings")) {
        foreach ($theme in @("light", "dark")) {
            $shot = "$screen-$theme"
            $file = Join-Path $Out "$shot@2x.png"
            $profile = Join-Path $profiles $shot
            # Edge reports "N bytes written" on stderr; under -ErrorAction
            # Stop, PowerShell 5.1 turns that into a fatal NativeCommandError.
            # The progress line is not an error, so it must not be treated as one.
            $ErrorActionPreference = 'Continue'
            & $edge --headless=new --disable-gpu `
                "--user-data-dir=$profile" `
                --window-size=1536,1024 `
                --force-device-scale-factor=2 `
                --force-prefers-reduced-motion `
                --hide-scrollbars `
                "--screenshot=$file" `
                --virtual-time-budget=6000 `
                "http://localhost:$port/tray/ui-dev/?shot=$shot" 2>&1 | Out-Null
            $ErrorActionPreference = 'Stop'
            if (Test-Path $file) {
                $px = [System.Drawing.Image]::FromFile($file)
                Write-Host ("{0,-18} {1}x{2}" -f "$shot@2x.png", $px.Width, $px.Height)
                $px.Dispose()
            } else {
                Write-Error "no screenshot for $shot"
            }
        }
    }
}
finally {
    Stop-Process -Id $server.Id -Force -Confirm:$false -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $profiles -ErrorAction SilentlyContinue
}

Write-Host "wrote $Out"

# Removes Portal Desktop installed by install-desktop.ps1 or its installer.
#   irm https://raw.githubusercontent.com/MStefa003/portal/main/uninstall-desktop.ps1 | iex
# The `portal` command line tool, files you sent or received, and settings
# are not touched.

$dest = Join-Path $env:LOCALAPPDATA 'Programs\Portal Desktop'
$uninstaller = Join-Path $dest 'uninstall.exe'

# `return`, not `exit`: under `irm | iex`, exit would close the user's window.
if (-not (Test-Path $uninstaller)) {
    Write-Host "Portal Desktop is not installed in $dest."
    return
}
# `_?=` runs it in place, so waiting for it waits for the removal.
Start-Process $uninstaller -ArgumentList '/S', "_?=$dest" -Wait
Remove-Item -Recurse -Force $dest -ErrorAction SilentlyContinue
if (Test-Path $dest) {
    Write-Host "Could not remove $dest. Close Portal and try again." -ForegroundColor Yellow
    return
}
Write-Host "Portal Desktop has been removed."
Write-Host "Your settings are kept in $env:APPDATA\portal (the command line tool uses them too)."

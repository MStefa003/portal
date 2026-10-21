# Removes Portal installed by install.ps1.
#   irm https://raw.githubusercontent.com/MStefa003/portal/main/uninstall.ps1 | iex
# Files you sent or received are not touched.

$dest = Join-Path $env:LOCALAPPDATA 'Programs\Portal'

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath) {
    $kept = ($userPath -split ';') | Where-Object { $_ -and $_.TrimEnd('\') -ne $dest }
    [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
}

# `return`, not `exit`: under `irm | iex`, exit would close the user's window.
if (Test-Path $dest) {
    try {
        Remove-Item -Recurse -Force $dest -ErrorAction Stop
    } catch {
        Write-Host "Could not remove $dest. Close any window where Portal is running and try again." -ForegroundColor Yellow
        return
    }
}
Write-Host "Portal has been removed. Open a new terminal window for the change to take effect."

# Installs the latest Portal release on Windows.
#   irm https://raw.githubusercontent.com/MStefa003/portal/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'

$repo = 'MStefa003/portal'
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$target = "$arch-pc-windows-msvc"

$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
$asset = $release.assets | Where-Object { $_.name -like "*-$target.zip" } | Select-Object -First 1
if (-not $asset) { throw "No Portal download for $target in $($release.tag_name)." }
$sum = $release.assets | Where-Object { $_.name -eq "$($asset.name).sha256" } | Select-Object -First 1

$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $zip = Join-Path $tmp $asset.name
    Write-Host "Downloading Portal $($release.tag_name) for $target..."
    Invoke-WebRequest $asset.browser_download_url -OutFile $zip -UseBasicParsing
    if ($sum) {
        $expected = (Invoke-RestMethod $sum.browser_download_url).Split(' ')[0].Trim()
        $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) { throw "Checksum mismatch for $($asset.name)." }
    }
    Expand-Archive $zip $tmp

    $dest = Join-Path $env:LOCALAPPDATA 'Programs\Portal'
    New-Item -ItemType Directory -Force $dest | Out-Null
    Get-ChildItem $tmp -Recurse -Filter *.exe | Copy-Item -Destination $dest -Force

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($userPath -split ';') -notcontains $dest) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest".TrimStart(';'), 'User')
        Write-Host "Added $dest to your PATH. Open a new terminal to use it."
    }
    Write-Host "Installed: $dest\portal.exe"
} finally {
    Remove-Item -Recurse -Force $tmp
}

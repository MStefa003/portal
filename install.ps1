# Installs the latest Portal release on Windows.
#   irm https://raw.githubusercontent.com/MStefa003/portal/main/install.ps1 | iex
$ErrorActionPreference = 'Stop'

$repo = 'MStefa003/portal'
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$target = "$arch-pc-windows-msvc"

# Older Windows PowerShell may not offer TLS 1.2, which GitHub requires.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# The newest version, from where github.com/<repo>/releases/latest redirects.
# Unlike the GitHub API, this has no hourly limit per IP address.
# `return`, not `exit`: under `irm | iex`, exit would close the user's window.
try {
    $request = [Net.WebRequest]::Create("https://github.com/$repo/releases/latest")
    $request.AllowAutoRedirect = $false
    $response = $request.GetResponse()
    $location = $response.Headers['Location']
    $response.Close()
} catch {
    $location = $null
}
if ($location -notmatch '/releases/tag/([^/?#]+)') {
    Write-Host "Could not find a Portal release to install. Check your internet connection, or see https://github.com/$repo/releases" -ForegroundColor Yellow
    return
}
$tag = $Matches[1]
$name = "portal-$tag-$target.zip"
$base = "https://github.com/$repo/releases/download/$tag"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $zip = Join-Path $tmp $name
    Write-Host "Downloading Portal $tag for $target..."
    try {
        Invoke-WebRequest "$base/$name" -OutFile $zip -UseBasicParsing
        Invoke-WebRequest "$base/$name.sha256" -OutFile "$zip.sha256" -UseBasicParsing
    } catch {
        Write-Host "Portal $tag has no download for $target yet. See https://github.com/$repo/releases" -ForegroundColor Yellow
        return
    }
    $expected = (Get-Content "$zip.sha256" -Raw).Trim().Split(' ')[0]
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "The download is damaged (checksum mismatch). Please try again." }
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

    # "Send with Portal" in Explorer's right-click menu, unless opted out.
    if (-not $env:PORTAL_NO_CONTEXT_MENU) {
        & "$dest\portal.exe" context-menu | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Write-Host "Added 'Send with Portal' to the right-click menu (remove it with: portal context-menu --remove)."
        }
    }
} finally {
    Remove-Item -Recurse -Force $tmp
}

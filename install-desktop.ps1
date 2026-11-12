# Installs the latest Portal Desktop release on Windows: the app with a
# window, for sending and receiving without the command line.
#   irm https://raw.githubusercontent.com/MStefa003/portal/main/install-desktop.ps1 | iex
# It does not install the `portal` command line tool (install.ps1 does), and
# that one does not install this. Remove it in Settings > Apps, or with
# uninstall-desktop.ps1.
$ErrorActionPreference = 'Stop'

$repo = 'MStefa003/portal'
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$target = "$arch-pc-windows-msvc"

# Older Windows PowerShell may not offer TLS 1.2, which GitHub requires.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# PORTAL_VERSION (e.g. v0.6.0) installs that release instead of the newest;
# PORTAL_DOWNLOAD_URL takes the files from another place (a mirror, or a
# test build).
if ($env:PORTAL_VERSION) {
    $tag = $env:PORTAL_VERSION
} else {
    # The newest version, from where github.com/<repo>/releases/latest
    # redirects. Unlike the GitHub API, this has no hourly limit per IP address.
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
}
$name = "portal-desktop-$tag-$target-setup.exe"
$base = if ($env:PORTAL_DOWNLOAD_URL) { $env:PORTAL_DOWNLOAD_URL.TrimEnd('/') } else { "https://github.com/$repo/releases/download/$tag" }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $setup = Join-Path $tmp $name
    Write-Host "Downloading Portal Desktop $tag for $target..."
    try {
        Invoke-WebRequest "$base/$name" -OutFile $setup -UseBasicParsing
        Invoke-WebRequest "$base/$name.sha256" -OutFile "$setup.sha256" -UseBasicParsing
    } catch {
        Write-Host "Portal $tag has no desktop app for $target yet. See https://github.com/$repo/releases" -ForegroundColor Yellow
        return
    }
    $expected = (Get-Content "$setup.sha256" -Raw).Trim().Split(' ')[0]
    $actual = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "The download is damaged (checksum mismatch). Please try again." }

    # The installer, without its windows: for this user only, no
    # administrator rights, with Start menu and desktop shortcuts.
    Write-Host "Installing..."
    $process = Start-Process $setup -ArgumentList '/S' -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "The installer failed (exit code $($process.ExitCode))." }

    $app = Join-Path $env:LOCALAPPDATA 'Programs\Portal Desktop\portal-desktop.exe'
    if (-not (Test-Path $app)) { throw "The installer finished, but $app is missing." }
    Write-Host "Installed Portal Desktop. Open it from the Start menu or the desktop shortcut."
    Start-Process $app
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Repository = if ($env:AIRWIRE_REPOSITORY) {
    $env:AIRWIRE_REPOSITORY
} else {
    "H34TB145T/airwire"
}
$Version = if ($env:AIRWIRE_VERSION) {
    $env:AIRWIRE_VERSION
} else {
    "latest"
}
$InstallDirectory = if ($env:AIRWIRE_INSTALL_DIR) {
    $env:AIRWIRE_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "Airwire\bin"
}

$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
switch ($Architecture) {
    "X64" { $Asset = "airwire-windows-x86_64.zip" }
    "Arm64" { $Asset = "airwire-windows-aarch64.zip" }
    default { throw "Unsupported processor architecture: $Architecture" }
}

if ($env:AIRWIRE_DOWNLOAD_BASE) {
    $DownloadBase = $env:AIRWIRE_DOWNLOAD_BASE.TrimEnd("/")
} elseif ($Version -eq "latest") {
    $DownloadBase = "https://github.com/$Repository/releases/latest/download"
} else {
    $DownloadBase = "https://github.com/$Repository/releases/download/$Version"
}

$TemporaryDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "airwire-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $TemporaryDirectory | Out-Null

try {
    $Archive = Join-Path $TemporaryDirectory $Asset
    $ChecksumFile = "$Archive.sha256"
    Write-Host "Downloading $Asset..."
    Invoke-WebRequest "$DownloadBase/$Asset" -OutFile $Archive
    Invoke-WebRequest "$DownloadBase/$Asset.sha256" -OutFile $ChecksumFile

    $ExpectedChecksum = ((Get-Content $ChecksumFile -Raw).Trim() -split "\s+")[0].ToLowerInvariant()
    $ActualChecksum = (Get-FileHash $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ActualChecksum -ne $ExpectedChecksum) {
        throw "SHA-256 verification failed for $Asset"
    }

    $Extracted = Join-Path $TemporaryDirectory "extracted"
    Expand-Archive -Path $Archive -DestinationPath $Extracted
    New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
    Copy-Item (Join-Path $Extracted "airwire.exe") (Join-Path $InstallDirectory "airwire.exe") -Force

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $PathEntries = @($UserPath -split ";" | Where-Object { $_ })
    if ($PathEntries -notcontains $InstallDirectory) {
        $UpdatedPath = (@($InstallDirectory) + $PathEntries) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $UpdatedPath, "User")
        Write-Host "Added $InstallDirectory to your user PATH."
    }
    if (($env:Path -split ";") -notcontains $InstallDirectory) {
        $env:Path = "$InstallDirectory;$env:Path"
    }

    & (Join-Path $InstallDirectory "airwire.exe") --version
    Write-Host "Installed Airwire to $(Join-Path $InstallDirectory 'airwire.exe')"
    Write-Host "Open a new terminal, then run: airwire --start"
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $TemporaryDirectory
}

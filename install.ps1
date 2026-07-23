$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$LocalAppData = $env:LOCALAPPDATA
if ([string]::IsNullOrWhiteSpace($LocalAppData)) {
    if ([string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        throw "Cannot determine the current user's local application-data directory"
    }
    $LocalAppData = Join-Path $env:USERPROFILE "AppData\Local"
}

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
    Join-Path $LocalAppData "Airwire\bin"
}
$TorInstallDirectory = if ($env:AIRWIRE_TOR_DIR) {
    $env:AIRWIRE_TOR_DIR
} else {
    Join-Path $LocalAppData "Airwire\tor-expert"
}

# This is Tor Project's stable, command-line-only Expert Bundle. Keep the
# version and checksum together when updating it.
$TorBundleVersion = "15.0.19"
$TorBundleAsset = "tor-expert-bundle-windows-x86_64-$TorBundleVersion.tar.gz"
$TorBundleChecksum = "6ac067402c7b4a3dc37887ed3754b3914b67fdc220c966190683e9ccf91abf0f"
$TorDownloadBase = "https://archive.torproject.org/tor-package-archive/torbrowser/$TorBundleVersion"

function Find-TorExecutable {
    $Candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($env:AIRWIRE_TOR_BINARY)) {
        $Candidates += $env:AIRWIRE_TOR_BINARY
    }
    $TorCommand = Get-Command "tor.exe" -ErrorAction SilentlyContinue
    if ($null -ne $TorCommand) {
        if (-not [string]::IsNullOrWhiteSpace($TorCommand.Source)) {
            $Candidates += $TorCommand.Source
        } elseif (-not [string]::IsNullOrWhiteSpace($TorCommand.Path)) {
            $Candidates += $TorCommand.Path
        }
    }
    $Candidates += (Join-Path $TorInstallDirectory "tor\tor.exe")
    $Candidates += (Join-Path $LocalAppData "Tor Browser\Browser\TorBrowser\Tor\tor.exe")

    if (-not [string]::IsNullOrWhiteSpace($env:PROGRAMFILES)) {
        $Candidates += (Join-Path $env:PROGRAMFILES "Tor Browser\Browser\TorBrowser\Tor\tor.exe")
    }
    $ProgramFilesX86 = [Environment]::GetEnvironmentVariable("ProgramFiles(x86)")
    if (-not [string]::IsNullOrWhiteSpace($ProgramFilesX86)) {
        $Candidates += (Join-Path $ProgramFilesX86 "Tor Browser\Browser\TorBrowser\Tor\tor.exe")
    }
    if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        $Candidates += (Join-Path $env:USERPROFILE "Desktop\Tor Browser\Browser\TorBrowser\Tor\tor.exe")
        $Candidates += (Join-Path $env:USERPROFILE "Downloads\Tor Browser\Browser\TorBrowser\Tor\tor.exe")
    }

    foreach ($Candidate in $Candidates) {
        if (-not [string]::IsNullOrWhiteSpace($Candidate) -and
            (Test-Path -LiteralPath $Candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $Candidate).Path
        }
    }
    return $null
}

function Assert-Sha256 {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Expected,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $ActualHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ([string]::IsNullOrWhiteSpace($ActualHash)) {
        throw "Cannot calculate the SHA-256 checksum for $Label"
    }
    if ($ActualHash.ToLowerInvariant() -ne $Expected.ToLowerInvariant()) {
        throw "SHA-256 verification failed for $Label"
    }
}

function Get-NormalizedPathEntry {
    param([string]$Entry)

    if ([string]::IsNullOrWhiteSpace($Entry)) {
        return ""
    }
    $Expanded = [Environment]::ExpandEnvironmentVariables($Entry.Trim())
    try {
        $Expanded = [System.IO.Path]::GetFullPath($Expanded)
    } catch {
        # Preserve legacy PATH text that cannot be normalized.
    }
    return $Expanded.TrimEnd([char[]]"\/")
}

$Architecture = $env:PROCESSOR_ARCHITEW6432
if ([string]::IsNullOrWhiteSpace($Architecture)) {
    $Architecture = $env:PROCESSOR_ARCHITECTURE
}
if ([string]::IsNullOrWhiteSpace($Architecture)) {
    throw "Cannot determine the Windows processor architecture"
}

switch ($Architecture.ToUpperInvariant()) {
    "AMD64" { $Asset = "airwire-windows-x86_64.zip" }
    "X64" { $Asset = "airwire-windows-x86_64.zip" }
    "ARM64" { $Asset = "airwire-windows-aarch64.zip" }
    default { throw "Unsupported processor architecture: $Architecture" }
}

if (-not [string]::IsNullOrWhiteSpace($env:AIRWIRE_DOWNLOAD_BASE)) {
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

    $ChecksumContents = Get-Content $ChecksumFile -Raw
    if ([string]::IsNullOrWhiteSpace($ChecksumContents)) {
        throw "Downloaded checksum file is empty for $Asset"
    }
    $ExpectedChecksum = (($ChecksumContents.Trim() -split "\s+")[0]).ToLowerInvariant()
    Assert-Sha256 -Path $Archive -Expected $ExpectedChecksum -Label $Asset

    $Extracted = Join-Path $TemporaryDirectory "extracted"
    Expand-Archive -Path $Archive -DestinationPath $Extracted
    $Executable = Join-Path $Extracted "airwire.exe"
    if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
        throw "Downloaded archive does not contain airwire.exe"
    }

    $TorExecutable = Find-TorExecutable
    if ([string]::IsNullOrWhiteSpace($TorExecutable)) {
        $TorArchive = Join-Path $TemporaryDirectory $TorBundleAsset
        Write-Host "Downloading Tor Expert Bundle $TorBundleVersion..."
        Invoke-WebRequest "$TorDownloadBase/$TorBundleAsset" -OutFile $TorArchive
        Assert-Sha256 -Path $TorArchive -Expected $TorBundleChecksum -Label $TorBundleAsset

        $TarCommand = Get-Command "tar.exe" -ErrorAction SilentlyContinue
        if ($null -eq $TarCommand) {
            $TarCommand = Get-Command "tar" -ErrorAction SilentlyContinue
        }
        if ($null -eq $TarCommand) {
            throw "Windows tar.exe is required to extract the Tor Expert Bundle"
        }
        $TarExecutable = $TarCommand.Source
        if ([string]::IsNullOrWhiteSpace($TarExecutable)) {
            $TarExecutable = $TarCommand.Path
        }
        if ([string]::IsNullOrWhiteSpace($TarExecutable)) {
            throw "Cannot determine the path to Windows tar.exe"
        }

        $TorExtracted = Join-Path $TemporaryDirectory "tor-extracted"
        New-Item -ItemType Directory -Force -Path $TorExtracted | Out-Null
        & $TarExecutable -xzf $TorArchive -C $TorExtracted
        if ($LASTEXITCODE -ne 0) {
            throw "Could not extract the Tor Expert Bundle (tar exit code $LASTEXITCODE)"
        }
        $ExtractedTorExecutable = Join-Path $TorExtracted "tor\tor.exe"
        if (-not (Test-Path -LiteralPath $ExtractedTorExecutable -PathType Leaf)) {
            throw "Tor Expert Bundle does not contain tor.exe"
        }

        New-Item -ItemType Directory -Force -Path $TorInstallDirectory | Out-Null
        Copy-Item (Join-Path $TorExtracted "*") $TorInstallDirectory -Recurse -Force
        $TorExecutable = Join-Path $TorInstallDirectory "tor\tor.exe"
        if (-not (Test-Path -LiteralPath $TorExecutable -PathType Leaf)) {
            throw "Tor installation did not create $TorExecutable"
        }
        $TorExecutable = (Resolve-Path -LiteralPath $TorExecutable).Path
        Write-Host "Installed Tor to $TorExecutable"
    } else {
        Write-Host "Using Tor at $TorExecutable"
    }

    [Environment]::SetEnvironmentVariable("AIRWIRE_TOR_BINARY", $TorExecutable, "User")
    $env:AIRWIRE_TOR_BINARY = $TorExecutable
    Write-Host "Configured AIRWIRE_TOR_BINARY for your user account."

    New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
    Copy-Item $Executable (Join-Path $InstallDirectory "airwire.exe") -Force

    # Always select the newly installed executable. Checking only whether this
    # directory exists in PATH can leave an older Cargo copy ahead of it.
    $NormalizedInstallDirectory = Get-NormalizedPathEntry $InstallDirectory
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $PathEntries = if ([string]::IsNullOrWhiteSpace($UserPath)) {
        @()
    } else {
        @($UserPath -split ";" | Where-Object { $_ })
    }
    $OtherUserPathEntries = @($PathEntries | Where-Object {
        -not [string]::Equals(
            (Get-NormalizedPathEntry $_),
            $NormalizedInstallDirectory,
            [System.StringComparison]::OrdinalIgnoreCase
        )
    })
    $UpdatedPath = (@($InstallDirectory) + $OtherUserPathEntries) -join ";"
    if ($UpdatedPath -ne $UserPath) {
        [Environment]::SetEnvironmentVariable("Path", $UpdatedPath, "User")
        Write-Host "Set $InstallDirectory as the first user PATH entry."
    }

    $CurrentPathEntries = @($env:Path -split ";" | Where-Object { $_ })
    $OtherCurrentPathEntries = @($CurrentPathEntries | Where-Object {
        -not [string]::Equals(
            (Get-NormalizedPathEntry $_),
            $NormalizedInstallDirectory,
            [System.StringComparison]::OrdinalIgnoreCase
        )
    })
    $env:Path = (@($InstallDirectory) + $OtherCurrentPathEntries) -join ";"

    & (Join-Path $InstallDirectory "airwire.exe") --version
    Write-Host "Installed Airwire to $(Join-Path $InstallDirectory 'airwire.exe')"
    Write-Host "Run: airwire --start"
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $TemporaryDirectory
}

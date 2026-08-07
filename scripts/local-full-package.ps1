param(
    [Parameter(Mandatory = $true)]
    [string]$RepoRoot
)

$ErrorActionPreference = 'Stop'
Set-Location $RepoRoot

function Test-Executable {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [string[]]$Arguments = @()
    )

    if (-not (Test-Path $Path)) {
        return $false
    }

    try {
        $output = & $Path @Arguments 2>&1
        $exitCode = $LASTEXITCODE
        if ($exitCode -eq 0) {
            return $true
        }

        Write-Host "[TOOL] $Path exited with code $exitCode." -ForegroundColor Yellow
        if ($output) {
            $output | Select-Object -First 6 | ForEach-Object {
                Write-Host "       $_" -ForegroundColor Yellow
            }
        }
        return $false
    } catch {
        Write-Host "[TOOL] Failed to run $Path : $($_.Exception.Message)" -ForegroundColor Yellow
        return $false
    }
}

function Test-LocalPort([int]$Port) {
    $client = New-Object System.Net.Sockets.TcpClient
    try {
        $async = $client.BeginConnect('127.0.0.1', $Port, $null, $null)
        if (-not $async.AsyncWaitHandle.WaitOne(250)) {
            return $false
        }
        $client.EndConnect($async)
        return $true
    } catch {
        return $false
    } finally {
        $client.Close()
    }
}

$releaseExe = Join-Path $RepoRoot 'target\release\biliup.exe'
if (-not (Test-Path $releaseExe)) {
    throw 'target\release\biliup.exe was not found'
}

$launcher = Get-ChildItem -Path $RepoRoot -Filter '*.bat' -File |
    Where-Object {
        $_.Name -like '*Live Replay*' -and
        $_.Name -notlike '*Quick*' -and
        $_.Name -notlike '*Full*'
    } |
    Select-Object -First 1
if (-not $launcher) {
    throw 'Live Replay launcher BAT was not found in repository root'
}

$requiredFiles = @(
    'LICENSE',
    'FFMPEG-NOTICE.txt',
    'README.md',
    'LIVE_REPLAY.md',
    'public\config.toml'
)
foreach ($relative in $requiredFiles) {
    if (-not (Test-Path (Join-Path $RepoRoot $relative))) {
        throw "Required package file is missing: $relative"
    }
}

$toolsRoot = Join-Path $RepoRoot 'target\live-replay-tools'
$ffmpegCache = Join-Path $toolsRoot 'ffmpeg'
$ffmpegExe = Join-Path $ffmpegCache 'ffmpeg.exe'
$ffprobeExe = Join-Path $ffmpegCache 'ffprobe.exe'

$cacheValid = (Test-Executable $ffmpegExe @('-version')) -and (Test-Executable $ffprobeExe @('-version'))
if ($cacheValid) {
    Write-Host '[FFMPEG] Reusing cached ffmpeg.exe and ffprobe.exe.'
} else {
    Write-Host '[FFMPEG] Cache missing or invalid. Downloading LGPL Windows build once...'
    New-Item -ItemType Directory -Force -Path $toolsRoot | Out-Null

    $downloadRoot = Join-Path $toolsRoot 'download'
    if (Test-Path $downloadRoot) {
        Remove-Item -Recurse -Force $downloadRoot
    }
    New-Item -ItemType Directory -Force -Path $downloadRoot | Out-Null

    $archive = Join-Path $downloadRoot 'ffmpeg-lgpl.zip'
    $extract = Join-Path $downloadRoot 'extract'
    $uri = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl.zip'

    Invoke-WebRequest -Uri $uri -OutFile $archive -UseBasicParsing
    Expand-Archive -Path $archive -DestinationPath $extract -Force

    $downloadedFfmpeg = Get-ChildItem -Path $extract -Filter 'ffmpeg.exe' -File -Recurse | Select-Object -First 1
    $downloadedFfprobe = Get-ChildItem -Path $extract -Filter 'ffprobe.exe' -File -Recurse | Select-Object -First 1
    if (-not $downloadedFfmpeg -or -not $downloadedFfprobe) {
        throw 'Downloaded FFmpeg archive does not contain ffmpeg.exe and ffprobe.exe'
    }

    if (Test-Path $ffmpegCache) {
        Remove-Item -Recurse -Force $ffmpegCache
    }
    New-Item -ItemType Directory -Force -Path $ffmpegCache | Out-Null
    Copy-Item $downloadedFfmpeg.FullName $ffmpegExe
    Copy-Item $downloadedFfprobe.FullName $ffprobeExe
    Remove-Item -Recurse -Force $downloadRoot

    if (-not (Test-Executable $ffmpegExe @('-version'))) {
        throw 'Cached ffmpeg.exe failed its version smoke test'
    }
    if (-not (Test-Executable $ffprobeExe @('-version'))) {
        throw 'Cached ffprobe.exe failed its version smoke test'
    }
    Write-Host '[FFMPEG] Download complete. Future Full builds will reuse this cache.'
}

$distRoot = Join-Path $RepoRoot 'dist'
$packageDir = Join-Path $distRoot 'live-replay'
$zipPath = Join-Path $distRoot 'live-replay-windows.zip'

New-Item -ItemType Directory -Force -Path $distRoot | Out-Null
if (Test-Path $packageDir) {
    Remove-Item -Recurse -Force $packageDir
}
New-Item -ItemType Directory -Force -Path $packageDir | Out-Null

Copy-Item $releaseExe (Join-Path $packageDir 'live-replay.exe')
Copy-Item $ffmpegExe (Join-Path $packageDir 'ffmpeg.exe')
Copy-Item $ffprobeExe (Join-Path $packageDir 'ffprobe.exe')
Copy-Item $launcher.FullName (Join-Path $packageDir $launcher.Name)
Copy-Item (Join-Path $RepoRoot 'LICENSE') (Join-Path $packageDir 'LICENSE')
Copy-Item (Join-Path $RepoRoot 'FFMPEG-NOTICE.txt') (Join-Path $packageDir 'FFMPEG-NOTICE.txt')
Copy-Item (Join-Path $RepoRoot 'README.md') (Join-Path $packageDir 'README.md')
Copy-Item (Join-Path $RepoRoot 'LIVE_REPLAY.md') (Join-Path $packageDir 'LIVE_REPLAY.md')
Copy-Item (Join-Path $RepoRoot 'public\config.toml') (Join-Path $packageDir 'config.example.toml')

if (-not (Test-Executable (Join-Path $packageDir 'ffmpeg.exe') @('-version'))) {
    throw 'Portable ffmpeg.exe could not start'
}
if (-not (Test-Executable (Join-Path $packageDir 'ffprobe.exe') @('-version'))) {
    throw 'Portable ffprobe.exe could not start'
}

if (Test-LocalPort 19159) {
    Write-Host '[SMOKE] Port 19159 is already in use. Skipping dashboard startup smoke test.'
} else {
    Write-Host '[SMOKE] Starting packaged live-replay.exe and checking dashboard...'
    $smokeDir = Join-Path $distRoot 'smoke'
    if (Test-Path $smokeDir) {
        Remove-Item -Recurse -Force $smokeDir
    }
    New-Item -ItemType Directory -Force -Path $smokeDir | Out-Null

    $oldNoBrowser = $env:LIVE_REPLAY_NO_BROWSER
    $env:LIVE_REPLAY_NO_BROWSER = '1'
    $process = Start-Process -FilePath (Join-Path $packageDir 'live-replay.exe') -WorkingDirectory $smokeDir -PassThru
    try {
        $ready = $false
        foreach ($attempt in 1..60) {
            Start-Sleep -Milliseconds 500
            if ($process.HasExited) {
                throw "Packaged live-replay.exe exited before dashboard startup. Exit code: $($process.ExitCode)"
            }
            try {
                $response = Invoke-WebRequest 'http://127.0.0.1:19159/' -UseBasicParsing -TimeoutSec 2
                if ($response.StatusCode -eq 200) {
                    $ready = $true
                    break
                }
            } catch {
            }
        }
        if (-not $ready) {
            throw 'Packaged live-replay.exe did not expose the dashboard on port 19159'
        }
        Write-Host '[SMOKE] Dashboard startup passed.'
    } finally {
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -Force
        }
        if ($null -eq $oldNoBrowser) {
            Remove-Item Env:LIVE_REPLAY_NO_BROWSER -ErrorAction SilentlyContinue
        } else {
            $env:LIVE_REPLAY_NO_BROWSER = $oldNoBrowser
        }
        if (Test-Path $smokeDir) {
            Remove-Item -Recurse -Force $smokeDir
        }
    }
}

if (Test-Path $zipPath) {
    Remove-Item -Force $zipPath
}
Compress-Archive -Path (Join-Path $packageDir '*') -DestinationPath $zipPath -CompressionLevel Optimal

Write-Host '[PACKAGE] Portable folder:' $packageDir
Write-Host '[PACKAGE] Portable ZIP:' $zipPath

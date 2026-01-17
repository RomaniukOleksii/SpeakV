$TARGET_TRIPLE = "x86_64-pc-windows-msvc"
$BIN_NAME = "speakv"

Write-Host "Starting release build..."

& powershell -ExecutionPolicy Bypass -File .\build.ps1

if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed!"
    exit $LASTEXITCODE
}

$BUILD_EXE_CLIENT = Join-Path $PSScriptRoot "target\release\$BIN_NAME.exe"
$BUILD_EXE_SERVER = Join-Path $PSScriptRoot "target\release\$BIN_NAME-server.exe"
$RELEASE_DIR = Join-Path $PSScriptRoot "dist"
$RELEASE_EXE_CLIENT = Join-Path $RELEASE_DIR "$BIN_NAME-$TARGET_TRIPLE.exe"
$RELEASE_EXE_SERVER = Join-Path $RELEASE_DIR "$BIN_NAME-server-$TARGET_TRIPLE.exe"

if (!(Test-Path $RELEASE_DIR)) {
    New-Item -ItemType Directory -Path $RELEASE_DIR | Out-Null
}

if (Test-Path $BUILD_EXE_CLIENT) {
    Copy-Item -Path $BUILD_EXE_CLIENT -Destination $RELEASE_EXE_CLIENT -Force
    Write-Host "Client release ready at: $RELEASE_EXE_CLIENT"
}

if (Test-Path $BUILD_EXE_SERVER) {
    Copy-Item -Path $BUILD_EXE_SERVER -Destination $RELEASE_EXE_SERVER -Force
    Write-Host "Server release ready at: $RELEASE_EXE_SERVER"
}
else {
    Write-Host "Error: Binaries not found!"
    exit 1
}

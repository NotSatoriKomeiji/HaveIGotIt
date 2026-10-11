param([string]$Version = "0.1.0")
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$') { throw "Invalid version" }
$ManifestVersion = (Select-String -Path Cargo.toml -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
if ($Version -ne $ManifestVersion) { throw "Update Cargo.toml and assets/app.rc/app.manifest before releasing another version" }
cargo test --locked
if ($LASTEXITCODE) { throw "Tests failed" }
cargo fmt --check
if ($LASTEXITCODE) { throw "Formatting failed" }
cargo clippy --locked --all-targets -- -D warnings
if ($LASTEXITCODE) { throw "Lint failed" }
New-Item -ItemType Directory -Force -Path dist | Out-Null
foreach ($Language in @("en", "zh")) {
    if ($Language -eq "zh") { cargo build --locked --release --features zh-release }
    else { cargo build --locked --release --no-default-features }
    if ($LASTEXITCODE) { throw "Build failed for $Language" }
    $Name = "HaveIGotIt-v$Version-windows-x64-$Language"
    $Stage = Join-Path dist $Name
    if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
    New-Item -ItemType Directory -Path $Stage | Out-Null
    Copy-Item target/release/haveigotit.exe (Join-Path $Stage "HaveIGotIt.exe")
    Copy-Item README.md, README_CN.md, LICENSE, THIRD_PARTY.md $Stage
    Copy-Item -Recurse licenses $Stage
    Copy-Item -Recurse docs $Stage
    $Zip = Join-Path dist "$Name.zip"
    if (Test-Path $Zip) { Remove-Item $Zip }
    Compress-Archive -Path $Stage -DestinationPath $Zip -CompressionLevel Optimal
}
$Lines = Get-ChildItem dist -Filter "HaveIGotIt-v$Version-windows-x64-*.zip" | Sort-Object Name | ForEach-Object {
    "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
}
$Lines | Set-Content -Encoding ascii (Join-Path dist "SHA256SUMS.txt")
Write-Host "Packages are ready in dist/. Test the Windows UI before publishing."


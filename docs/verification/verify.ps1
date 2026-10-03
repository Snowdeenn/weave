param(
    [string]$Revision,
    [string]$OutputPath = "target/verification/traceability.json"
)
$ErrorActionPreference = "Stop"
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
Push-Location $root
try {
    function Git-Read {
        param([string[]]$GitArgs)
        $result = & git @GitArgs
        if ($LASTEXITCODE -ne 0) { throw "git $GitArgs failed ($LASTEXITCODE)" }
        return $result
    }
    $manifest = Get-Content (Join-Path $PSScriptRoot "features.json") -Raw | ConvertFrom-Json
    if (!$Revision) { $Revision = $manifest.reference_revision }
    $reference = Git-Read @("rev-parse", "--verify", "$Revision^{commit}")
    if ($reference -ne $manifest.reference_revision) {
        throw "Manifest pinned to $($manifest.reference_revision); update and review it before auditing $reference."
    }
    $head = Git-Read @("rev-parse", "HEAD")
    $tracked = @(Git-Read @("ls-files"))
    $currentCode = @($tracked | Where-Object { $_ -match "^(src|examples|tests|benchmarks/src)/.*\.rs$" })
    # Include untracked Rust files too: additions must not escape the inventory.
    $untrackedCode = @(Git-Read @("ls-files", "--others", "--exclude-standard") |
        Where-Object { $_ -match "^(src|examples|tests|benchmarks/src)/.*\.rs$" })
    $currentCode = @($currentCode + $untrackedCode | Sort-Object -Unique)
    $baseline = @(Git-Read @("ls-tree", "-r", $reference)) | ForEach-Object {
        $parts = $_ -split "\s+", 4
        [ordered]@{ path = $parts[3]; git_blob = $parts[2] }
    }
    $testIndex = @()
    foreach ($file in $currentCode) {
        if (!(Test-Path -LiteralPath $file)) { continue }
        $content = Get-Content -LiteralPath $file -Raw
        $matches = [regex]::Matches($content,
            '(?m)^\s*#\[test\]\s*(?<attributes>(?:#\[[^\r\n]*\]\s*)*)fn\s+(?<name>\w+)')
        foreach ($match in $matches) {
            $testIndex += [pscustomobject]@{
                id = "$file#$($match.Groups['name'].Value)"
                file = $file
                name = $match.Groups['name'].Value
                ignored = $match.Groups['attributes'].Value -match '#\[ignore'
                # This describes source gating, not evidence that assertions ran.
                linux_only = $file -match '^src/(memory/(linux|buffer)|affinity/linux|topology/linux)\.rs$|^tests/affinity\.rs$'
            }
        }
    }
    $associations = @()
    $mappedSources = @()
    $mappedTests = @()
    foreach ($feature in $manifest.features) {
        $mappedSources += $feature.sources
        $selected = @()
        foreach ($selector in $feature.tests) {
            $found = @($testIndex | Where-Object { $_.id -like $selector })
            if (!$found.Count) { throw "Unresolved test selector: $selector ($($feature.id))" }
            $selected += $found
        }
        $selected = @($selected | Sort-Object id -Unique)
        $mappedTests += $selected.id
        foreach ($source in $feature.sources) {
            if ($source -notin $currentCode -or !(Test-Path -LiteralPath $source)) { throw "Missing mapped source: $source" }
        }
        $associations += [ordered]@{
            id = $feature.id
            contract = $feature.contract
            sources = $feature.sources
            tests = $selected
            doctests = @($feature.doctests)
            checks = @($feature.checks)
            platform = $feature.platform
            limits = @($feature.limits)
        }
    }
    $unmappedSources = @($currentCode | Where-Object { $_ -notmatch '^tests/' -and $_ -notin $mappedSources })
    $unmappedTests = @($testIndex | Where-Object { $_.id -notin $mappedTests })
    if ($unmappedSources.Count -or $unmappedTests.Count) {
        throw "Unmapped sources: $unmappedSources; unmapped tests: $($unmappedTests.id)"
    }
    $report = [ordered]@{
        reference_revision = $reference
        working_head = $head
        generated_at_utc = [DateTime]::UtcNow.ToString("o")
        note = $manifest.note
        baseline_files = @($baseline)
        working_code_files = @($currentCode | ForEach-Object {
            if (Test-Path -LiteralPath $_) {
                [ordered]@{ path = $_; sha256 = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash }
            }
        })
        changed_tracked_files = @(Git-Read @("diff", "--name-only", $reference, "--"))
        working_status = @(Git-Read @("status", "--short"))
        discovered_tests = $testIndex.Count
        ignored_tests = @($testIndex | Where-Object ignored).Count
        features = $associations
    }
    $parent = Split-Path $OutputPath -Parent
    if ($parent) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    $report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    Write-Output "Traceability valid: $($associations.Count) features, $($testIndex.Count) declared tests, $($report.ignored_tests) ignored."
    Write-Output "Reference: $reference; HEAD: $head; report: $OutputPath"
    Write-Output "This checks associations and revision identity; it does not execute tests or measure line coverage."
} finally {
    Pop-Location
}

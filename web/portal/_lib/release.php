<?php
// Shared logic for the ICU download page and its release API.
//
// Trust model: the API token only gates who may *upload*. What becomes
// visible is decided by the release signatures, verified here with
// ssh-keygen against allowed_signers kept outside the webroot:
//   SHA256SUMS.sig        namespace icu-release
//   latest.json payload   namespace icu-update (what the Windows updater checks)
// The private key never reaches this server.
//
// Published bytes are immutable. downloads/latest.json and
// downloads/<version>/<file> remain plain files served by Apache, so the
// updater URLs never pass through PHP.

declare(strict_types=1);

const ICU_VERSION_RE = '/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/';
const ICU_NAME_RE = '/^[A-Za-z0-9][A-Za-z0-9_.+\-]{0,179}$/';
const ICU_META = ['SHA256SUMS', 'SHA256SUMS.sig', 'latest.json'];
const ICU_BUILDINFO = ['BUILDINFO.linux', 'BUILDINFO.windows', 'BUILDINFO.windows-installer'];
const ICU_MAX_FILE = 256 * 1024 * 1024;
const ICU_MAX_META = 1024 * 1024;

// (pattern, label, kind, os, format), mirrors CLASSES in web/tools/publish-release.py.
// {v} matches any stable version so a package that carries a different
// version than its release is still recognised and can be flagged.
const ICU_CLASSES = [
    ['icu-{v}-windows-x86_64-setup\.exe', 'Windows · Installer', 'client', 'windows', 'setup'],
    ['icu-{v}-windows-x86_64\.zip', 'Windows · Portable ZIP', 'client', 'windows', 'zip'],
    ['icu(?:-client)?-{v}-\d+\.fc(\d+)\.x86_64\.rpm', 'Fedora {n} · RPM', 'client', 'fedora', 'rpm'],
    ['icu_{v}-\d+\+deb(\d+)_amd64\.deb', 'Debian {n} · DEB', 'client', 'debian', 'deb'],
    ['icu-server-{v}-\d+\.x86_64\.rpm', 'Server · RPM (Fedora, AlmaLinux 10+)', 'server', 'linux-rpm', 'rpm'],
    ['icu-server_{v}-\d+_amd64\.deb', 'Server · DEB (Debian 12+)', 'server', 'linux-deb', 'deb'],
];

final class ReleaseError extends RuntimeException
{
    public function __construct(string $message, public readonly int $status = 422)
    {
        parent::__construct($message);
    }
}

function icu_config(): array
{
    static $config = null;
    if ($config === null) {
        $file = getenv('ICU_PORTAL_CONFIG') ?: dirname(__DIR__, 3) . '/private/icu-portal/config.php';
        $loaded = is_file($file) ? require $file : [];
        $root = dirname(__DIR__);
        $config = array_merge([
            'downloads' => $root . '/downloads',
            'data' => dirname($file),
            'token_sha256' => '',
            'allowed_signers' => dirname($file) . '/allowed_signers',
            'ssh_keygen' => '/usr/bin/ssh-keygen',
            'server_note' => '',
        ], is_array($loaded) ? $loaded : []);
    }
    return $config;
}

function icu_version_tuple(string $version): array
{
    if (!preg_match(ICU_VERSION_RE, $version, $m)) {
        throw new ReleaseError('only stable major.minor.patch versions are allowed');
    }
    return [(int) $m[1], (int) $m[2], (int) $m[3]];
}

function icu_version_cmp(string $a, string $b): int
{
    return icu_version_tuple($a) <=> icu_version_tuple($b);
}

/** Classify a package name; 'version' is the version its name carries. */
function icu_classify(string $name): ?array
{
    $v = '((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))';
    foreach (ICU_CLASSES as [$pattern, $label, $kind, $os, $format]) {
        if (preg_match('/^' . str_replace('{v}', $v, $pattern) . '$/', $name, $m)) {
            $n = $m[2] ?? '';
            return [
                'version' => $m[1], 'label' => str_replace('{n}', $n, $label),
                'kind' => $kind, 'os' => $os . $n, 'format' => $format,
            ];
        }
    }
    return null;
}

/** Releases on disk, newest first. Reads manifest.json, or latest.json (0.1.28 layout). */
function icu_releases(): array
{
    $dir = icu_config()['downloads'];
    $out = [];
    foreach (@scandir($dir) ?: [] as $entry) {
        if (!preg_match(ICU_VERSION_RE, $entry) || is_link("$dir/$entry") || !is_dir("$dir/$entry")) {
            continue;
        }
        $manifest = null;
        foreach (['manifest.json', 'latest.json'] as $candidate) {
            $path = "$dir/$entry/$candidate";
            if (is_file($path) && ($data = json_decode((string) file_get_contents($path), true)) && is_array($data)) {
                $manifest = $data;
                break;
            }
        }
        $out[$entry] = icu_describe($entry, $manifest, "$dir/$entry");
    }
    uksort($out, fn($a, $b) => icu_version_cmp($b, $a));
    return $out;
}

/** Normalise one release for the page and the API, checking each file on disk. */
function icu_describe(string $version, ?array $manifest, string $path): array
{
    $files = [];
    foreach (($manifest['files'] ?? []) as $file) {
        $name = (string) ($file['name'] ?? '');
        if (!preg_match(ICU_NAME_RE, $name)) {
            continue;
        }
        $class = icu_classify($name);
        $disk = "$path/$name";
        $files[] = [
            'name' => $name,
            'label' => $class['label'] ?? $name,
            'kind' => $class['kind'] ?? ($file['kind'] ?? 'other'),
            'os' => $file['os'] ?? ($class['os'] ?? ''),
            'format' => $file['format'] ?? ($class['format'] ?? ''),
            'size' => (int) ($file['size'] ?? 0),
            'sha256' => (string) ($file['sha256'] ?? ''),
            // The version the package itself carries, from its file name.
            'version' => $class['version'] ?? null,
            'version_matches' => ($class['version'] ?? null) === $version,
            'present' => is_file($disk) && !is_link($disk) && filesize($disk) === (int) ($file['size'] ?? -1),
            'url' => "downloads/$version/" . rawurlencode($name),
        ];
    }
    usort($files, fn($a, $b) => [$a['kind'] !== 'client', icu_sort_key($a)] <=> [$b['kind'] !== 'client', icu_sort_key($b)]);
    return [
        'version' => $version,
        'published' => $manifest['published'] ?? null,
        'commit' => $manifest['commit'] ?? null,
        'complete' => (bool) ($manifest['complete'] ?? false),
        'manifest' => $manifest !== null,
        'files' => $files,
        'sums' => is_file("$path/SHA256SUMS") ? "downloads/$version/SHA256SUMS" : null,
        'sig' => is_file("$path/SHA256SUMS.sig") ? "downloads/$version/SHA256SUMS.sig" : null,
        'notes' => icu_notes($version),
    ];
}

function icu_sort_key(array $file): array
{
    $order = ['windows' => 0, 'fedora' => 1, 'debian' => 2, 'linux-rpm' => 3, 'linux-deb' => 4];
    $family = preg_replace('/\d+$/', '', $file['os']);
    $n = (int) preg_replace('/^\D+/', '', $file['os'] . '0');
    return [$order[$family] ?? 9, -$n, $file['format'] === 'zip'];
}

/** The channel the updater sees; null if missing or unreadable. */
function icu_latest(): ?array
{
    $path = icu_config()['downloads'] . '/latest.json';
    $data = is_file($path) ? json_decode((string) file_get_contents($path), true) : null;
    return is_array($data) && isset($data['version']) && preg_match(ICU_VERSION_RE, (string) $data['version']) ? $data : null;
}

// ---- release notes (not signed, editable) -------------------------------

function icu_notes_path(string $version): string
{
    icu_version_tuple($version);
    return icu_config()['data'] . "/notes/$version.txt";
}

function icu_notes(string $version): ?string
{
    $path = icu_notes_path($version);
    $text = is_file($path) ? trim((string) file_get_contents($path)) : '';
    return $text === '' ? null : $text;
}

function icu_set_notes(string $version, string $text): void
{
    if (strlen($text) > 16384 || !mb_check_encoding($text, 'UTF-8')) {
        throw new ReleaseError('notes must be UTF-8 text up to 16 KiB');
    }
    $path = icu_notes_path($version);
    @mkdir(dirname($path), 0750, true);
    icu_atomic_write($path, rtrim($text) . "\n", 0640);
}

// ---- authentication -----------------------------------------------------

function icu_authorized(): bool
{
    $expected = strtolower((string) icu_config()['token_sha256']);
    $header = $_SERVER['HTTP_AUTHORIZATION'] ?? $_SERVER['REDIRECT_HTTP_AUTHORIZATION'] ?? '';
    if (!preg_match('/^[0-9a-f]{64}$/', $expected) || !preg_match('/^Bearer\s+(\S{32,256})$/', $header, $m)) {
        return false;
    }
    return hash_equals($expected, hash('sha256', $m[1]));
}

// ---- staging uploads ----------------------------------------------------

function icu_staging_root(): string
{
    return icu_config()['data'] . '/staging';
}

function icu_staging(string $id): string
{
    if (!preg_match('/^[0-9a-f]{32}$/', $id) || !is_dir(icu_staging_root() . "/$id")) {
        throw new ReleaseError('unknown upload', 404);
    }
    return icu_staging_root() . "/$id";
}

function icu_staging_create(string $version): string
{
    icu_version_tuple($version);
    $id = bin2hex(random_bytes(16));
    $dir = icu_staging_root() . "/$id/files";
    if (!mkdir($dir, 0750, true)) {
        throw new ReleaseError('cannot create staging directory', 500);
    }
    file_put_contents(icu_staging_root() . "/$id/version", $version);
    return $id;
}

function icu_staging_version(string $id): string
{
    return trim((string) file_get_contents(icu_staging($id) . '/version'));
}

/** Stream one file body into staging. Never trust the name beyond the whitelist. */
function icu_staging_put(string $id, string $name, $input): array
{
    $dir = icu_staging($id) . '/files';
    if (!preg_match(ICU_NAME_RE, $name) || !(in_array($name, ICU_META, true)
            || in_array($name, ICU_BUILDINFO, true) || icu_classify($name) !== null)) {
        throw new ReleaseError("file name not allowed: $name");
    }
    $limit = in_array($name, ICU_META, true) || str_starts_with($name, 'BUILDINFO.') ? ICU_MAX_META : ICU_MAX_FILE;
    $tmp = "$dir/.part-" . bin2hex(random_bytes(8));
    $out = fopen($tmp, 'xb');
    $ctx = hash_init('sha256');
    $size = 0;
    try {
        while (!feof($input)) {
            $chunk = fread($input, 1 << 20);
            if ($chunk === false) {
                throw new ReleaseError('read error', 400);
            }
            $size += strlen($chunk);
            if ($size > $limit) {
                throw new ReleaseError("file too large: $name", 413);
            }
            hash_update($ctx, $chunk);
            fwrite($out, $chunk);
        }
        fflush($out);
        fclose($out);
        $out = null;
        if ($size === 0) {
            throw new ReleaseError("empty file: $name");
        }
        chmod($tmp, 0644);
        rename($tmp, "$dir/$name");
    } finally {
        if ($out) {
            fclose($out);
        }
        @unlink($tmp);
    }
    return ['name' => $name, 'size' => $size, 'sha256' => hash_final($ctx)];
}

function icu_rmtree(string $dir): void
{
    if (!is_dir($dir) || is_link($dir)) {
        @unlink($dir);
        return;
    }
    foreach (array_diff(scandir($dir) ?: [], ['.', '..']) as $entry) {
        icu_rmtree("$dir/$entry");
    }
    @rmdir($dir);
}

/** Drop abandoned uploads after a day. */
function icu_staging_gc(): void
{
    foreach (glob(icu_staging_root() . '/*', GLOB_ONLYDIR) ?: [] as $dir) {
        if (filemtime($dir) < time() - 86400) {
            icu_rmtree($dir);
        }
    }
}

// ---- verification -------------------------------------------------------

function icu_ssh_verify(string $data, string $signature, string $namespace): void
{
    $config = icu_config();
    if (!is_file($config['allowed_signers'])) {
        throw new ReleaseError('server has no allowed_signers', 500);
    }
    $sig = tempnam($config['data'], '.sig-');
    file_put_contents($sig, $signature);
    try {
        $proc = proc_open(
            [$config['ssh_keygen'], '-Y', 'verify', '-f', $config['allowed_signers'],
             '-I', 'icu-release', '-n', $namespace, '-s', $sig],
            [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']],
            $pipes
        );
        if (!is_resource($proc)) {
            throw new ReleaseError('cannot run ssh-keygen', 500);
        }
        fwrite($pipes[0], $data);
        fclose($pipes[0]);
        stream_get_contents($pipes[1]);
        $err = stream_get_contents($pipes[2]);
        fclose($pipes[1]);
        fclose($pipes[2]);
        if (proc_close($proc) !== 0) {
            throw new ReleaseError("$namespace signature rejected: " . trim((string) $err));
        }
    } finally {
        @unlink($sig);
    }
}

/**
 * Check a staged release completely. Returns [latest.json bytes, decoded, names].
 * Mirrors release_manifest() in web/tools/publish-release.py.
 */
function icu_verify_staging(string $files, string $version): array
{
    foreach (ICU_META as $meta) {
        if (!is_file("$files/$meta")) {
            throw new ReleaseError("missing $meta");
        }
    }
    $sums = (string) file_get_contents("$files/SHA256SUMS");
    icu_ssh_verify($sums, (string) file_get_contents("$files/SHA256SUMS.sig"), 'icu-release');

    $listed = [];
    $packages = [];
    $commits = [];
    foreach (preg_split('/\n/', rtrim($sums, "\n")) as $line) {
        if (!preg_match('/^([a-f0-9]{64}) [ *]([A-Za-z0-9][A-Za-z0-9_.+\-]{0,179})$/', $line, $m)) {
            throw new ReleaseError('malformed SHA256SUMS line');
        }
        [, $digest, $name] = $m;
        if (isset($listed[$name])) {
            throw new ReleaseError("duplicate artifact: $name");
        }
        $path = "$files/$name";
        if (!is_file($path) || hash_file('sha256', $path) !== $digest) {
            throw new ReleaseError("$name is missing or does not match SHA256SUMS");
        }
        $listed[$name] = $digest;
        if (str_starts_with($name, 'BUILDINFO.')) {
            $lines = preg_split('/\r?\n/', (string) file_get_contents($path));
            if (in_array('DIRTY', $lines, true)) {
                throw new ReleaseError('a dirty build must never be published');
            }
            $ids = array_values(array_filter($lines, fn($l) => str_starts_with($l, 'commit ')));
            if (count($ids) !== 1 || !preg_match('/^commit [a-f0-9]{40}$/', $ids[0])) {
                throw new ReleaseError("invalid build provenance: $name");
            }
            $commits[substr($ids[0], 7)] = true;
            continue;
        }
        $class = icu_classify($name);
        if ($class === null || $class['version'] !== $version) {
            throw new ReleaseError("$name does not belong to release $version");
        }
        $packages[$name] = ['size' => filesize($path), 'sha256' => $digest] + $class;
    }
    $staged = array_diff(scandir($files) ?: [], ['.', '..']);
    foreach ($staged as $name) {
        if (!in_array($name, ICU_META, true) && !isset($listed[$name])) {
            throw new ReleaseError("unlisted file uploaded: $name");
        }
    }
    $have = [];
    foreach ($packages as $p) {
        $have[$p['kind'] . '/' . $p['os'] . '/' . $p['format']] = true;
    }
    $required = ['client/windows/zip', 'client/windows/setup', 'client/fedora42/rpm', 'client/fedora43/rpm',
                 'client/debian13/deb', 'server/linux-rpm/rpm', 'server/linux-deb/deb'];
    if (count($commits) !== 1 || array_diff(ICU_BUILDINFO, array_keys($listed))
            || array_diff($required, array_keys($have))) {
        throw new ReleaseError('release incomplete or built from different commits');
    }

    $raw = (string) file_get_contents("$files/latest.json");
    $latest = json_decode($raw, true);
    $auth = is_array($latest) ? ($latest['authenticated'] ?? null) : null;
    if (!is_array($auth) || !is_string($auth['payload'] ?? null) || !is_string($auth['signature'] ?? null)) {
        throw new ReleaseError('latest.json carries no authenticated payload');
    }
    $payloadBytes = base64_decode($auth['payload'], true);
    if ($payloadBytes === false) {
        throw new ReleaseError('authenticated payload is not base64');
    }
    icu_ssh_verify($payloadBytes, $auth['signature'], 'icu-update');
    $payload = json_decode($payloadBytes, true);
    $visible = $latest;
    unset($visible['authenticated']);
    if (!is_array($payload) || icu_canonical($payload) !== icu_canonical($visible)) {
        throw new ReleaseError('latest.json fields differ from the signed payload');
    }
    if (($payload['schema'] ?? null) !== 1 || ($payload['version'] ?? null) !== $version
            || ($payload['complete'] ?? null) !== true || ($payload['dirty'] ?? null) !== false
            || ($payload['base'] ?? null) !== "downloads/$version/"
            || !isset($commits[$payload['commit'] ?? ''])) {
        throw new ReleaseError('signed payload does not describe this release');
    }
    $signed = [];
    foreach ($payload['files'] ?? [] as $f) {
        $signed[(string) ($f['name'] ?? '')] = [(int) ($f['size'] ?? -1), (string) ($f['sha256'] ?? '')];
    }
    if (count($signed) !== count($packages)) {
        throw new ReleaseError('signed payload lists other packages than SHA256SUMS');
    }
    foreach ($packages as $name => $p) {
        if (($signed[$name] ?? null) !== [$p['size'], $p['sha256']]) {
            throw new ReleaseError("signed payload disagrees on $name");
        }
    }
    return [$raw, $latest, array_keys($listed)];
}

function icu_canonical(mixed $value): mixed
{
    if (is_array($value)) {
        if (!array_is_list($value)) {
            ksort($value);
        }
        return array_map('icu_canonical', $value);
    }
    return $value;
}

// ---- publishing ---------------------------------------------------------

function icu_atomic_write(string $path, string $data, int $mode = 0644): void
{
    $tmp = dirname($path) . '/.icu-publish-' . bin2hex(random_bytes(6));
    $fh = fopen($tmp, 'xb');
    try {
        fwrite($fh, $data);
        fflush($fh);
        if (function_exists('fsync')) {
            fsync($fh);
        }
        fclose($fh);
        $fh = null;
        chmod($tmp, $mode);
        if (!rename($tmp, $path)) {
            throw new ReleaseError("cannot replace $path", 500);
        }
    } finally {
        if ($fh) {
            fclose($fh);
        }
        @unlink($tmp);
    }
}

/**
 * Verify and publish a staged upload. Order: version directory complete
 * first (one rename), latest.json last (one rename). Republishing identical
 * bytes is a no-op; anything else that would change published bytes fails.
 */
function icu_commit(string $id, bool $makeLatest = true): array
{
    $stage = icu_staging($id);
    $version = icu_staging_version($id);
    $downloads = icu_config()['downloads'];
    $lock = fopen("$downloads/.publish.lock", 'c');
    if (!$lock || !flock($lock, LOCK_EX | LOCK_NB)) {
        throw new ReleaseError('another publish is running', 409);
    }
    try {
        [$raw, , $names] = icu_verify_staging("$stage/files", $version);
        $current = icu_latest();
        if ($makeLatest && $current && icu_version_cmp($current['version'], $version) > 0) {
            throw new ReleaseError("refusing to downgrade latest.json from {$current['version']}", 409);
        }
        $dest = "$downloads/$version";
        if (is_link($dest)) {
            throw new ReleaseError('version directory must not be a symlink', 500);
        }
        $keep = array_merge($names, ['SHA256SUMS', 'SHA256SUMS.sig']);
        if (is_dir($dest)) {
            // Already published: only identical bytes are accepted.
            foreach ($keep as $name) {
                if (!is_file("$dest/$name") || hash_file('sha256', "$dest/$name") !== hash_file('sha256', "$stage/files/$name")) {
                    throw new ReleaseError("published artifact is immutable: $name", 409);
                }
            }
            $published = is_file("$dest/manifest.json") ? "$dest/manifest.json" : "$dest/latest.json";
            if (!is_file($published) || (string) file_get_contents($published) !== $raw) {
                throw new ReleaseError("$version is already published with other signed metadata", 409);
            }
        } else {
            $files = "$stage/files";
            rename("$files/latest.json", "$stage/latest.json");
            file_put_contents("$files/manifest.json", $raw);
            foreach (array_diff(scandir($files) ?: [], ['.', '..']) as $name) {
                chmod("$files/$name", 0644);
            }
            chmod($files, 0755);
            if ((@stat($files)['dev'] ?? 0) !== (@stat($downloads)['dev'] ?? 1)) {
                throw new ReleaseError('staging and downloads are on different file systems', 500);
            }
            if (!rename($files, $dest)) {
                throw new ReleaseError('cannot publish version directory', 500);
            }
        }
        if ($makeLatest && (!$current || ($current['version'] !== $version)
                || (string) file_get_contents("$downloads/latest.json") !== $raw)) {
            icu_atomic_write("$downloads/latest.json", $raw);
        }
    } finally {
        flock($lock, LOCK_UN);
        fclose($lock);
    }
    icu_rmtree($stage);
    return icu_describe($version, json_decode($raw, true), "$downloads/$version");
}

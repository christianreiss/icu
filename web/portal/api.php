<?php
// ICU release API, routed here by .htaccess as api/v1/<path>.
//
// Public (no token):
//   GET    api/v1/releases                       all releases on disk, newest first
//   GET    api/v1/releases/latest                the release latest.json points to
//   GET    api/v1/releases/<ver>                 one release
// Token (Authorization: Bearer <token>):
//   POST   api/v1/uploads          {"version": "x.y.z"}   -> {"id": ...}
//   PUT    api/v1/uploads/<id>/<file>   raw body, one file per request
//   POST   api/v1/uploads/<id>/commit   {"latest": true}  verify and publish
//   DELETE api/v1/uploads/<id>
//   PUT    api/v1/releases/<ver>/notes  text/plain, "Neu in" text for the page
// Downloads stay static: downloads/<ver>/<file> and downloads/latest.json.

declare(strict_types=1);
require __DIR__ . '/_lib/release.php';

header('Content-Type: application/json; charset=utf-8');
header('Cache-Control: no-store');
header('X-Content-Type-Options: nosniff');

function reply(int $status, array $body): never
{
    http_response_code($status);
    echo json_encode($body, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE), "\n";
    exit;
}

function json_body(): array
{
    $raw = file_get_contents('php://input', false, null, 0, 65536);
    $data = $raw === '' ? [] : json_decode((string) $raw, true);
    if (!is_array($data)) {
        throw new ReleaseError('request body must be a JSON object', 400);
    }
    return $data;
}

function require_token(): void
{
    if (!icu_authorized()) {
        header('WWW-Authenticate: Bearer realm="icu-release"');
        throw new ReleaseError('authentication required', 401);
    }
}

$method = $_SERVER['REQUEST_METHOD'] ?? 'GET';
$path = trim((string) ($_GET['path'] ?? ''), '/');
$parts = $path === '' ? [] : explode('/', $path);

try {
    switch (true) {
        case $method === 'GET' && $parts === ['releases']:
            $latest = icu_latest();
            reply(200, ['latest' => $latest['version'] ?? null, 'releases' => array_values(icu_releases())]);

        case $method === 'GET' && count($parts) === 2 && $parts[0] === 'releases':
            $version = $parts[1] === 'latest' ? (icu_latest()['version'] ?? '') : $parts[1];
            $releases = icu_releases();
            if (!isset($releases[$version])) {
                throw new ReleaseError('no such release', 404);
            }
            reply(200, $releases[$version] + ['latest' => $version === (icu_latest()['version'] ?? null)]);

        case $method === 'PUT' && count($parts) === 3 && $parts[0] === 'releases' && $parts[2] === 'notes':
            require_token();
            if (!isset(icu_releases()[$parts[1]])) {
                throw new ReleaseError('no such release', 404);
            }
            icu_set_notes($parts[1], (string) file_get_contents('php://input', false, null, 0, 16385));
            reply(200, ['version' => $parts[1], 'notes' => icu_notes($parts[1])]);

        case $method === 'POST' && $parts === ['uploads']:
            require_token();
            icu_staging_gc();
            $version = (string) (json_body()['version'] ?? '');
            reply(201, ['id' => icu_staging_create($version), 'version' => $version]);

        case $method === 'PUT' && count($parts) === 3 && $parts[0] === 'uploads':
            require_token();
            icu_staging($parts[1]);
            $in = fopen('php://input', 'rb');
            reply(200, icu_staging_put($parts[1], rawurldecode($parts[2]), $in));

        case $method === 'POST' && count($parts) === 3 && $parts[0] === 'uploads' && $parts[2] === 'commit':
            require_token();
            @set_time_limit(300);
            $makeLatest = (bool) (json_body()['latest'] ?? true);
            reply(200, icu_commit($parts[1], $makeLatest) + ['latest' => $makeLatest]);

        case $method === 'DELETE' && count($parts) === 2 && $parts[0] === 'uploads':
            require_token();
            icu_rmtree(icu_staging($parts[1]));
            reply(200, ['deleted' => $parts[1]]);

        default:
            throw new ReleaseError('not found', 404);
    }
} catch (ReleaseError $e) {
    reply($e->status, ['error' => $e->getMessage()]);
} catch (Throwable $e) {
    error_log('icu api: ' . $e);
    reply(500, ['error' => 'internal error']);
}

<?php
// Copy to web20/private/icu-portal/config.php (outside the webroot), mode 0640.
// token_sha256: sha256 hex of the API token; the token itself lives only in
// the fleet secret store (icu.release-api-token), never on this server.
return [
    'token_sha256' => '',
    // 'downloads' => '/var/www/clients/client1/web20/web/icu/downloads',
    // 'allowed_signers' => __DIR__ . '/allowed_signers',  // copy of packaging/allowed_signers
    // 'ssh_keygen' => '/usr/bin/ssh-keygen',
    'server_note' => 'Server: lab.uggs.io',
    // Writes (uploads, notes) only from these networks (REMOTE_ADDR); empty = any.
    // 'write_networks' => ['10.2.0.0/24', '2a01:1f8:2000:5::1/128'],  // owlan + NAT66 of aurora
];

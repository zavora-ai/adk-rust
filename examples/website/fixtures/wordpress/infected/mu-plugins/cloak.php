<?php
/**
 * Plugin Name: Inert cloaking marker
 * Description: Spec 3.5 §11 S3 fixture. Adds a script tag for crawlers only, pointing at a
 * `.test` domain that never resolves; browsers get the page unchanged. Inert.
 */
add_action('wp_head', function () {
    $agent = strtolower($_SERVER['HTTP_USER_AGENT'] ?? '');
    if (is_front_page() && (str_contains($agent, 'bot') || str_contains($agent, 'adk-site'))) {
        echo '<script src="https://cdn-stats.test/c.js"></script>';
    }
});

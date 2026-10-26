<?php
/**
 * Plugin Name: Website fixture
 * Description: Serves the fixture on loopback under its public origin (spec 3.5 §11): no
 * canonical redirect to https://solar.test, so the crawler's mapped requests are answered.
 */
remove_action('template_redirect', 'redirect_canonical');

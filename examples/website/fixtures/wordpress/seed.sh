#!/bin/sh
# Seeds the WordPress fixture with the content the recordings in ../recorded/ hold
# (spec 3.5 §11): a solar installer with posts, pages, a contact form and a small shop.
set -eu
cd /var/www/html
wp core is-installed || wp core install --url=https://solar.test --title="Example Solar" \
  --admin_user=owner --admin_password=fixture-only --admin_email=owner@solar.test --skip-email
wp option update blogdescription "Rooftop solar for homes in Nairobi"
wp option update timezone_string "Africa/Nairobi"
wp rewrite structure '/%year%/%monthnum%/%day%/%postname%/' --hard

# A contact-form plugin and a small shop, current releases from wordpress.org.
wp plugin install contact-form-7 --activate
wp plugin install woocommerce --activate
wp option update woocommerce_currency KES

wp term create category News --slug=news || true
wp post create --post_type=post --post_status=publish --post_name=hello-sun \
  --post_date="2026-10-01 08:00:00" --post_title="Why we started" --post_category=news \
  --post_content="<p>Example Solar started with one roof in Karen and a promise to size every system to the home it serves.</p>"
wp post create --post_type=post --post_status=publish --post_name=panel-care \
  --post_date="2026-10-12 08:00:00" --post_title="Caring for your panels" \
  --post_content="<p>Dust costs a panel up to a fifth of its output in the dry season. Rinse panels with clean water early in the morning, never with soap, and never climb onto the roof yourself.</p><p>We include two cleaning visits a year with every installation.</p>"
wp post create --post_type=page --post_status=publish --post_name=about --post_title="About us" \
  --post_content="<p>We are a team of six electricians and two engineers in Nairobi.</p>"
FORM=$(wp post list --post_type=wpcf7_contact_form --field=ID --posts_per_page=1)
wp post create --post_type=page --post_status=publish --post_name=contact --post_title="Contact" \
  --post_content="<p>Tell us about your roof.</p>[contact-form-7 id=\"$FORM\"]"
wp post create --post_type=page --post_status=private --post_name=private --post_title="Staff notes" \
  --post_content="<p>Internal.</p>"
wp wc product create --user=owner --name="Panel 400 W" --slug=panel-400 --sku=P-400 \
  --regular_price=25000 --description="A 400 W monocrystalline panel with a 25-year output warranty."
wp wc product create --user=owner --name="Battery 5 kWh" --slug=battery-5 --sku=B-5 \
  --regular_price=180000 --description="A 5 kWh lithium iron phosphate battery."
# The legacy redirect the recordings keep.
wp plugin install redirection --activate
wp redirection add /old-offers/ /products/panel-400/ --code=301 || true
echo "seeded"

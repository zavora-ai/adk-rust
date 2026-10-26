#!/bin/sh
# Applies the `infected` overlay's content markers (spec 3.5 §11, S3): inert — hidden links to
# `.test` domains that never resolve, and a script that decodes harmless text.
set -eu
cd /var/www/html
HELLO=$(wp post list --name=hello-sun --post_type=post --field=ID)
wp post update "$HELLO" --post_content="<p>Example Solar started with one roof in Karen and a promise to size every system to the home it serves.</p><div style=\"display:none\"><a href=\"https://cheap-casino.test/\">casino</a> <a href=\"https://cheap-casino.test/bonus\">bonus</a> <a href=\"https://pills.test/\">pills</a></div>"
ABOUT=$(wp post list --name=about --post_type=page --field=ID)
wp post update "$ABOUT" --post_content="<p>We are a team of six electricians and two engineers in Nairobi.</p><script>eval(atob('Y29uc29sZS5sb2coJ2luZXJ0IHRlc3QgbWFya2VyJyk='))</script>"
echo "infected (inert markers)"

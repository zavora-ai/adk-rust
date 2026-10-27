- **No hidden-link finding for WordPress theme markup** (`adk-site` `migrate`, spec 3.5 §7.8
  "Sanitiser"): a `<style>` rule now hides an element only when its selector names the element
  alone (a tag, classes and an id), outside `@media`, `@supports` and `@container`, and no other
  rule shows that class or id again, so the navigation block's
  `.wp-block-navigation__container .is-responsive { display: none }` no longer marks the whole
  menu hidden and responsive menus and dropdowns are page states. The hidden-link detector
  reports links to other sites only: skip links, menus and WooCommerce's `hidden` "View cart"
  link stay on the site, while injected link spam points away from it.

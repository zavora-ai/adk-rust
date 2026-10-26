/**
 * Wake-ups on the phone (spec 3.3 §5.4): the service worker, the browser's permission and its
 * push subscription to the kernel's own key. Every step starts from a tap.
 */
import { fromBase64url } from "./passkey";

/** The service worker, which shows the fixed words of a wake-up and acknowledges it. */
export async function serviceWorker(): Promise<ServiceWorkerRegistration | undefined> {
  if (!("serviceWorker" in navigator)) return undefined;
  return navigator.serviceWorker.register("/ui/sw.js", { scope: "/ui/" });
}

/** Whether `subscription` was made with the push key `key`. */
function madeWith(subscription: PushSubscription, key: Uint8Array): boolean {
  const own = subscription.options?.applicationServerKey;
  if (!own) return false;
  const bytes = new Uint8Array(own);
  return bytes.length === key.length && bytes.every((byte, index) => byte === key[index]);
}

/**
 * The browser's push subscription for `pushKey`, asking for permission first. A subscription
 * made with another key — the kernel's key file was replaced — is ended and made again, since
 * the push service refuses wake-ups signed with any key but its own.
 */
export async function subscribe(pushKey: string): Promise<PushSubscriptionJSON | undefined> {
  if (typeof Notification === "undefined" || !("PushManager" in window)) return undefined;
  const permission = await Notification.requestPermission();
  if (permission !== "granted") return undefined;
  const registration = await serviceWorker();
  if (!registration) return undefined;
  await navigator.serviceWorker.ready;
  const key = new Uint8Array(fromBase64url(pushKey));
  const existing = await registration.pushManager.getSubscription();
  if (existing && madeWith(existing, key)) return existing.toJSON();
  await existing?.unsubscribe();
  const subscription = await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
  return subscription.toJSON();
}

/** Ends this browser's push subscription, so nothing wakes it after Sign out. */
export async function unsubscribe(): Promise<void> {
  if (!("serviceWorker" in navigator)) return;
  const registration = await navigator.serviceWorker.getRegistration("/ui/");
  const subscription = await registration?.pushManager.getSubscription();
  await subscription?.unsubscribe();
}

/** Shows `count` on the app's icon where the system supports it. */
export function badge(count: number) {
  const own = navigator as Navigator & {
    setAppBadge?: (count?: number) => Promise<void>;
    clearAppBadge?: () => Promise<void>;
  };
  void (count > 0 ? own.setAppBadge?.(count) : own.clearAppBadge?.())?.catch(() => undefined);
}

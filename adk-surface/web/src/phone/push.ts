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

/** The browser's push subscription for `pushKey`, asking for permission first. */
export async function subscribe(pushKey: string): Promise<PushSubscriptionJSON | undefined> {
  if (typeof Notification === "undefined" || !("PushManager" in window)) return undefined;
  const permission = await Notification.requestPermission();
  if (permission !== "granted") return undefined;
  const registration = await serviceWorker();
  if (!registration) return undefined;
  await navigator.serviceWorker.ready;
  const subscription =
    (await registration.pushManager.getSubscription()) ??
    (await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: fromBase64url(pushKey) }));
  return subscription.toJSON();
}

/** Shows `count` on the app's icon where the system supports it. */
export function badge(count: number) {
  const own = navigator as Navigator & {
    setAppBadge?: (count?: number) => Promise<void>;
    clearAppBadge?: () => Promise<void>;
  };
  void (count > 0 ? own.setAppBadge?.(count) : own.clearAppBadge?.())?.catch(() => undefined);
}

/**
 * Images a person sends with a message (spec 3.3 §5.1). The engine checks each image's bytes
 * and takes at most four PNG, JPEG, WebP or GIF images of up to 3 MB each; the page keeps to
 * the same limits and shrinks a large picture before sending it, so a phone photo fits.
 */
import type { ImageData } from "./generated/api";

/** The most images one message carries: the engine's `MAX_IMAGES`. */
export const MAX_IMAGES = 4;

/** The most bytes one image has: the engine's `MAX_IMAGE_BYTES`. */
export const MAX_IMAGE_BYTES = 3 * 1024 * 1024;

/** The types the engine takes; never SVG. */
export const IMAGE_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif"] as const;

/** The longest side a picture is shrunk to when it is too large to send as it is. */
const LONGEST_SIDE = 1600;

/** An image waiting in the message box: what is sent, and a URL for its thumbnail. */
export type Attached = ImageData & { url: string };

/** The URL an image of a thread or the message box is shown from. */
export function imageUrl(image: ImageData): string {
  return `data:${image.mimeType};base64,${image.data}`;
}

async function base64(blob: Blob): Promise<string> {
  const url = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error ?? new Error("unreadable"));
    reader.readAsDataURL(blob);
  });
  return url.slice(url.indexOf(",") + 1);
}

/** `file` as JPEG with its longest side at most `LONGEST_SIDE`, where the browser can draw it. */
async function shrink(file: Blob): Promise<Blob | undefined> {
  if (typeof createImageBitmap !== "function" || typeof document === "undefined") return undefined;
  try {
    const bitmap = await createImageBitmap(file);
    const scale = Math.min(1, LONGEST_SIDE / Math.max(bitmap.width, bitmap.height));
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(bitmap.width * scale));
    canvas.height = Math.max(1, Math.round(bitmap.height * scale));
    const context = canvas.getContext("2d");
    if (!context) return undefined;
    context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    bitmap.close?.();
    return await new Promise<Blob | undefined>((resolve) => canvas.toBlob((blob) => resolve(blob ?? undefined), "image/jpeg", 0.88));
  } catch {
    return undefined;
  }
}

/** Why a file was left out, in the words the message box shows. */
export type Refusal = "type" | "size" | "count";

/**
 * The images among `files` that can go with a message already holding `have`: each read as
 * base64, a large still picture shrunk first, and what was left out with why.
 */
export async function readImages(files: File[], have: number): Promise<{ images: Attached[]; refused: Refusal[] }> {
  const images: Attached[] = [];
  const refused: Refusal[] = [];
  for (const file of files) {
    if (!(IMAGE_TYPES as readonly string[]).includes(file.type)) {
      refused.push("type");
      continue;
    }
    if (have + images.length >= MAX_IMAGES) {
      refused.push("count");
      continue;
    }
    let blob: Blob = file;
    let mimeType = file.type;
    // An animated GIF keeps its frames: it goes as it is, or not at all.
    if (file.size > MAX_IMAGE_BYTES && file.type !== "image/gif") {
      const smaller = await shrink(file);
      if (smaller) {
        blob = smaller;
        mimeType = "image/jpeg";
      }
    }
    if (blob.size > MAX_IMAGE_BYTES) {
      refused.push("size");
      continue;
    }
    const data = await base64(blob);
    images.push({ mimeType, data, url: `data:${mimeType};base64,${data}` });
  }
  return { images, refused };
}

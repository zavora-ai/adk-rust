/**
 * A scene drawn with WebGL (spec 3.3 §6.1 `genui`, extension catalogs), loaded only when a
 * drawing has one: `three` stays out of the home's main bundle. It draws what the server
 * checked and nothing else: the home's palette, no textures, models, fonts or shaders, labels
 * and panels as page text over the canvas. Picking an object fills the drawing's choice field
 * through the home's own field state; it never sends.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import type { DrawnScene, SceneObject } from "../../generated/api";
import { visible } from "../../safe";
import { recall, remember } from "./memory";
import type { Remembered } from "./memory";
import type { Palette, Pose, Vec3 } from "./plan";
import { cameraFor, isPicked, motionAt, objectColor, rampColor, readPalette, seriesColor, tween } from "./plan";

/** A panel to show beside an object: what the home draws for the held node. */
export interface CanvasPanel {
  key: string;
  anchor: string;
  content: ReactNode;
}

export interface SceneCanvasProps {
  scene: DrawnScene;
  /** The scene's surface and node, for what the page remembers between versions. */
  memoryKey: string;
  version: number;
  /** The values of the field picking fills. */
  picked: unknown;
  /** Picking is open: the scene has a field to fill and the form can still change. */
  pickable: boolean;
  onPick?: (value: string, label: string) => void;
  panels: CanvasPanel[];
  paused: boolean;
  reducedMotion: boolean;
  onFail: (why: string) => void;
  onSlow: () => void;
  onTouch?: () => void;
}

/** A label drawn as page text, anchored in the scene. */
interface Tag {
  key: string;
  text: string;
  className: string;
}

/** The labels of a scene: objects' labels, label shapes, axis names and ticks. */
function tagsOf(scene: DrawnScene): Tag[] {
  const tags: Tag[] = [];
  for (const object of scene.objects) {
    if (object.shape.type === "label") {
      tags.push({
        key: `shape:${object.id}`,
        text: visible(object.shape.text),
        className: `drawn-scene-label drawn-scene-label-${object.shape.size ?? "medium"}`,
      });
    } else if (object.label) {
      tags.push({ key: `object:${object.id}`, text: visible(object.label), className: "drawn-scene-label drawn-scene-label-small" });
    }
  }
  const axes = scene.axes;
  for (const axis of ["x", "y", "z"] as const) {
    const name = axes?.[axis];
    if (name) tags.push({ key: `axis:${axis}`, text: visible(name), className: "drawn-scene-axis" });
  }
  (axes?.ticks ?? []).forEach((tick, index) => {
    tags.push({ key: `tick:${index}`, text: visible(tick.text), className: "drawn-scene-tick" });
  });
  return tags;
}

const DEG = Math.PI / 180;
const n = (value: unknown, fallback = 0) => {
  const number = Number(value);
  return Number.isFinite(number) ? number : fallback;
};
const vec = (value: unknown, fallback: Vec3 = [0, 0, 0]): Vec3 =>
  Array.isArray(value) && value.length === 3 ? [n(value[0]), n(value[1]), n(value[2])] : fallback;

/** A moving object and how it stands at rest. */
interface Mover {
  node: THREE.Object3D;
  motion: NonNullable<DrawnScene["motion"]>[number];
  rest: Pose;
}

/** An object easing from its last version's place to this one's. */
interface Easing {
  node: THREE.Object3D;
  from: Remembered;
  to: Remembered;
  bars?: { mesh: THREE.InstancedMesh; at: number[]; width: number; from: number[]; to: number[] };
}

/** A material of an object new in this version, fading in to its own opacity. */
interface Appearing {
  material: THREE.Material;
  opacity: number;
  transparent: boolean;
}

interface World {
  renderer: THREE.WebGLRenderer;
  scene: THREE.Scene;
  camera: THREE.PerspectiveCamera;
  controls: OrbitControls;
  nodes: Map<string, THREE.Object3D>;
  /** Where each tag and panel sits, in the world. */
  anchors: Map<string, () => THREE.Vector3>;
  /** Meshes a pick can land on, and the object each picks. */
  pickables: THREE.Object3D[];
  picks: Map<THREE.Object3D, SceneObject>;
  outlines: Map<string, { lines: THREE.LineSegments[]; materials: THREE.MeshStandardMaterial[] }>;
  movers: Mover[];
  easing: Easing[];
  appearing: Appearing[];
  home: { position: THREE.Vector3; target: THREE.Vector3 };
  rest: Map<string, Remembered>;
  dispose: () => void;
}

/** Builds the world of `scene` on `canvas` in `palette`. */
function build(
  scene: DrawnScene,
  canvas: HTMLCanvasElement,
  palette: Palette,
  memoryKey: string,
  version: number,
  reduced: boolean,
): World {
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true, powerPreference: "low-power" });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
  const world = new THREE.Scene();
  const disposables: { dispose: () => void }[] = [];
  const keep = <T extends { dispose: () => void }>(thing: T): T => {
    disposables.push(thing);
    return thing;
  };
  const color = (css: string) => new THREE.Color(css);

  // Light: the preset, whose soft light always stays, then up to three more.
  const lighting = scene.lighting ?? "studio";
  if (lighting === "flat") {
    world.add(new THREE.AmbientLight(0xffffff, 2.6));
  } else {
    world.add(new THREE.HemisphereLight(0xffffff, color(palette["--scene-ground"]), lighting === "soft" ? 2.2 : 1.3));
    if (lighting === "studio") {
      const key = new THREE.DirectionalLight(0xffffff, 1.7);
      key.position.set(6, 11, 8);
      const fill = new THREE.DirectionalLight(0xffffff, 0.55);
      fill.position.set(-7, 4, -5);
      world.add(key, fill);
    } else {
      world.add(new THREE.AmbientLight(0xffffff, 0.5));
    }
  }
  for (const light of scene.lights ?? []) {
    const tint = light.color
      ? color(objectColor({ color: light.color, finish: "matte" }, undefined, palette))
      : new THREE.Color(0xffffff);
    const intensity = Math.min(2, Math.max(0, n(light.intensity, 1)));
    const [x, y, z] = vec(light.at);
    const added =
      light.kind === "directional"
        ? new THREE.DirectionalLight(tint, intensity)
        : light.kind === "spot"
          ? new THREE.SpotLight(tint, intensity * 3, 0, Math.PI / 6, 0.4, 0)
          : new THREE.PointLight(tint, intensity * 3, 0, 0);
    added.position.set(x, y, z);
    world.add(added);
  }

  const material = (object: SceneObject, override?: string): THREE.Material => {
    const look = object.look ?? { color: "chart1", finish: "matte" };
    const opacity = Math.min(1, Math.max(0.15, n(look.opacity, 1)));
    const options = {
      color: color(override ?? objectColor(look, object.state, palette)),
      transparent: opacity < 1,
      opacity,
      wireframe: look.wireframe === true,
      side: THREE.DoubleSide,
    };
    if (look.finish === "flat") return keep(new THREE.MeshBasicMaterial(options));
    return keep(
      new THREE.MeshStandardMaterial({
        ...options,
        roughness: look.finish === "glossy" ? 0.3 : 0.82,
        metalness: look.finish === "glossy" ? 0.12 : 0,
      }),
    );
  };

  const nodes = new Map<string, THREE.Object3D>();
  const picks = new Map<THREE.Object3D, SceneObject>();
  const pickables: THREE.Object3D[] = [];
  const outlines = new Map<string, { lines: THREE.LineSegments[]; materials: THREE.MeshStandardMaterial[] }>();
  const barMeshes = new Map<string, { mesh: THREE.InstancedMesh; at: number[]; width: number; heights: number[] }>();
  const selected = keep(new THREE.LineBasicMaterial({ color: color(palette["--scene-selected"]) }));

  const placeBars = (mesh: THREE.InstancedMesh, at: number[], width: number, heights: number[]) => {
    const matrix = new THREE.Matrix4();
    const rotation = new THREE.Quaternion();
    heights.forEach((height, index) => {
      const up = Math.max(Math.abs(height), 0.001);
      matrix.compose(
        new THREE.Vector3(at[index * 2], height < 0 ? height : 0, at[index * 2 + 1]),
        rotation,
        new THREE.Vector3(width, up, width),
      );
      mesh.setMatrixAt(index, matrix);
    });
    mesh.instanceMatrix.needsUpdate = true;
    mesh.computeBoundingSphere();
  };

  for (const object of scene.objects) {
    const node = new THREE.Group();
    node.name = object.id;
    const [x, y, z] = vec(object.at);
    node.position.set(x, y, z);
    if (object.turn) {
      const [a, b, c] = vec(object.turn);
      node.rotation.set(a * DEG, b * DEG, c * DEG);
    }
    if (object.scale) {
      const [a, b, c] = vec(object.scale, [1, 1, 1]);
      node.scale.set(a, b, c);
    }
    const shape = object.shape;
    const meshes: THREE.Mesh[] = [];
    const add = (geometry: THREE.BufferGeometry, override?: string) => {
      const mesh = new THREE.Mesh(keep(geometry), material(object, override));
      node.add(mesh);
      meshes.push(mesh);
      return mesh;
    };
    switch (shape.type) {
      case "box":
        add(new THREE.BoxGeometry(...vec(shape.size, [1, 1, 1])));
        break;
      case "sphere":
        add(new THREE.SphereGeometry(n(shape.radius, 0.5), 32, 16));
        break;
      case "cylinder":
        add(new THREE.CylinderGeometry(n(shape.radiusTop), n(shape.radiusBottom, 0.5), n(shape.height, 1), 32));
        break;
      case "torus":
        add(new THREE.TorusGeometry(n(shape.radius, 0.4), n(shape.tube, 0.15), 16, 64));
        break;
      case "plane": {
        const geometry = new THREE.PlaneGeometry(n(shape.width, 1), n(shape.depth, 1));
        geometry.rotateX(-Math.PI / 2);
        add(geometry);
        break;
      }
      case "globe": {
        const radius = n(shape.radius, 4);
        add(new THREE.SphereGeometry(radius, 48, 32));
        const graticule: THREE.Vector3[] = [];
        const at = (lat: number, lon: number) =>
          new THREE.Vector3(
            radius * 1.003 * Math.cos(lat * DEG) * Math.sin(lon * DEG),
            radius * 1.003 * Math.sin(lat * DEG),
            radius * 1.003 * Math.cos(lat * DEG) * Math.cos(lon * DEG),
          );
        for (let lat = -60; lat <= 60; lat += 30) {
          for (let lon = 0; lon < 360; lon += 6) graticule.push(at(lat, lon), at(lat, lon + 6));
        }
        for (let lon = 0; lon < 360; lon += 30) {
          for (let lat = -90; lat < 90; lat += 6) graticule.push(at(lat, lon), at(lat + 6, lon));
        }
        const lines = new THREE.LineSegments(
          keep(new THREE.BufferGeometry().setFromPoints(graticule)),
          keep(new THREE.LineBasicMaterial({ color: color(palette["--scene-muted"]), transparent: true, opacity: 0.55 })),
        );
        node.add(lines);
        break;
      }
      case "line": {
        let points: THREE.Vector3[] = [];
        for (let index = 0; index + 2 < shape.points.length; index += 3) {
          points.push(new THREE.Vector3(n(shape.points[index]), n(shape.points[index + 1]), n(shape.points[index + 2])));
        }
        if (shape.smooth && !shape.segments && points.length > 2) {
          points = new THREE.CatmullRomCurve3(points).getPoints(points.length * 8);
        }
        const tint = color(objectColor(object.look, object.state, palette));
        const lineMaterial = shape.dashed
          ? keep(new THREE.LineDashedMaterial({ color: tint, dashSize: 0.3, gapSize: 0.18 }))
          : keep(new THREE.LineBasicMaterial({ color: tint }));
        const geometry = keep(new THREE.BufferGeometry().setFromPoints(points));
        const line = shape.segments ? new THREE.LineSegments(geometry, lineMaterial) : new THREE.Line(geometry, lineMaterial);
        if (shape.dashed) line.computeLineDistances();
        node.add(line);
        const head = (tip: THREE.Vector3, from: THREE.Vector3) => {
          const cone = new THREE.Mesh(
            keep(new THREE.ConeGeometry(0.13, 0.38, 16)),
            keep(new THREE.MeshBasicMaterial({ color: tint })),
          );
          const direction = tip.clone().sub(from).normalize();
          cone.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), direction);
          cone.position.copy(tip).addScaledVector(direction, -0.19);
          node.add(cone);
        };
        if (points.length >= 2 && (shape.arrow === "end" || shape.arrow === "both")) {
          head(points[points.length - 1], points[points.length - 2]);
        }
        if (points.length >= 2 && shape.arrow === "both") head(points[0], points[1]);
        break;
      }
      case "bars": {
        const heights = shape.heights.map((height) => n(height));
        const at = shape.at.map((value) => n(value));
        const width = n(shape.width, 0.5);
        const geometry = new THREE.BoxGeometry(1, 1, 1);
        geometry.translate(0, 0.5, 0);
        const look = { ...(object.look ?? { color: "chart1", finish: "matte" }), color: "chart1" as const };
        const mesh = new THREE.InstancedMesh(keep(geometry), material({ ...object, look, state: null }, "#ffffff"), heights.length);
        placeBars(mesh, at, width, heights);
        heights.forEach((_, index) => {
          const series = shape.series?.[index];
          mesh.setColorAt(
            index,
            color(series == null ? objectColor(object.look, object.state, palette) : seriesColor(series, palette)),
          );
        });
        if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
        node.add(mesh);
        barMeshes.set(object.id, { mesh, at, width, heights });
        break;
      }
      case "points": {
        const count = Math.floor(shape.at.length / 3);
        const geometry = new THREE.SphereGeometry(1, 12, 8);
        const mesh = new THREE.InstancedMesh(keep(geometry), material({ ...object, state: null }, "#ffffff"), count);
        const matrix = new THREE.Matrix4();
        const size = n(shape.size, 0.1);
        for (let index = 0; index < count; index++) {
          matrix
            .makeScale(size, size, size)
            .setPosition(n(shape.at[index * 3]), n(shape.at[index * 3 + 1]), n(shape.at[index * 3 + 2]));
          mesh.setMatrixAt(index, matrix);
          const series = shape.series?.[index];
          mesh.setColorAt(
            index,
            color(series == null ? objectColor(object.look, object.state, palette) : seriesColor(series, palette)),
          );
        }
        if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
        mesh.computeBoundingSphere();
        node.add(mesh);
        break;
      }
      case "grid": {
        const columns = Math.max(2, shape.columns);
        const rows = Math.max(2, shape.rows);
        const geometry = new THREE.PlaneGeometry(n(shape.width, 8), n(shape.depth, 8), columns - 1, rows - 1);
        geometry.rotateX(-Math.PI / 2);
        const heights = shape.heights.map((height) => n(height));
        const low = Math.min(...heights);
        const high = Math.max(...heights);
        const position = geometry.getAttribute("position") as THREE.BufferAttribute;
        const colors: number[] = [];
        for (let index = 0; index < position.count; index++) {
          const height = heights[index] ?? 0;
          position.setY(index, height);
          const tint = color(rampColor(high > low ? (height - low) / (high - low) : 0.5, palette));
          colors.push(tint.r, tint.g, tint.b);
        }
        geometry.setAttribute("color", new THREE.Float32BufferAttribute(colors, 3));
        geometry.computeVertexNormals();
        const surface = new THREE.Mesh(
          keep(geometry),
          keep(
            new THREE.MeshStandardMaterial({
              vertexColors: true,
              roughness: 0.85,
              side: THREE.DoubleSide,
              wireframe: object.look?.wireframe === true,
            }),
          ),
        );
        node.add(surface);
        break;
      }
      default:
        break;
    }
    if (object.value) {
      const lines: THREE.LineSegments[] = [];
      const materials: THREE.MeshStandardMaterial[] = [];
      for (const mesh of meshes) {
        const edges = new THREE.LineSegments(keep(new THREE.EdgesGeometry(mesh.geometry)), selected);
        edges.scale.setScalar(1.02);
        edges.visible = false;
        mesh.add(edges);
        lines.push(edges);
        if (mesh.material instanceof THREE.MeshStandardMaterial) materials.push(mesh.material);
        picks.set(mesh, object);
        pickables.push(mesh);
      }
      outlines.set(object.id, { lines, materials });
    }
    nodes.set(object.id, node);
  }

  const root = new THREE.Group();
  for (const object of scene.objects) {
    const node = nodes.get(object.id);
    if (!node) continue;
    const parent = object.parent ? nodes.get(object.parent) : undefined;
    (parent ?? root).add(node);
  }
  world.add(root);
  root.updateMatrixWorld(true);

  // The camera fits what there is, from the preset's side, or stays where the person left it.
  // What there is includes labels, which have no geometry, and the axes' names and ticks.
  const bounds = new THREE.Box3().setFromObject(root);
  for (const object of scene.objects) {
    if (object.shape.type === "label") bounds.expandByPoint(nodes.get(object.id)!.getWorldPosition(new THREE.Vector3()));
  }
  const axisPoints = axisAnchors(scene, bounds);
  const fit = bounds.clone();
  for (const point of axisPoints.values()) fit.expandByPoint(point);
  const sphere = fit.getBoundingSphere(new THREE.Sphere());
  const centre: Vec3 = fit.isEmpty() ? [0, 0, 0] : [sphere.center.x, sphere.center.y, sphere.center.z];
  const fitted = cameraFor(scene, centre, fit.isEmpty() ? 4 : sphere.radius);
  const camera = new THREE.PerspectiveCamera(fitted.fov, 1, 0.05, Math.max(200, sphere.radius * 20));
  const remembered = recall(memoryKey);
  const start =
    remembered?.camera && remembered.version < version
      ? remembered.camera
      : { position: fitted.position, target: fitted.target };
  camera.position.set(...start.position);
  const controls = new OrbitControls(camera, canvas);
  controls.target.set(...start.target);
  controls.enabled = scene.camera?.fixed !== true;
  controls.enableDamping = !reduced;
  controls.autoRotateSpeed = 0.8;
  controls.update();

  // Axes: a floor and the names and ticks, as page text.
  const anchors = new Map<string, () => THREE.Vector3>();
  const axes = scene.axes;
  if (axes?.floor && !bounds.isEmpty()) {
    const size = Math.max(bounds.max.x - bounds.min.x, bounds.max.z - bounds.min.z) * 1.15 || 8;
    const grid = new THREE.GridHelper(size, 10, color(palette["--chart-axis"]), color(palette["--chart-grid"]));
    grid.position.set(sphere.center.x, Math.min(0, bounds.min.y), sphere.center.z);
    keep(grid.geometry);
    world.add(grid);
  }
  for (const [key, point] of axisPoints) anchors.set(key, () => point);
  // Labels follow their objects, above them; panels sit under them.
  const scratch = new THREE.Vector3();
  for (const object of scene.objects) {
    const node = nodes.get(object.id);
    if (!node) continue;
    const box = new THREE.Box3().setFromObject(node);
    const lift = box.isEmpty() ? 0.25 : box.max.y - node.getWorldPosition(scratch).y + 0.2;
    const drop = box.isEmpty() ? 0 : node.getWorldPosition(scratch).y - box.min.y;
    // A line is named at its middle point; anything else above its top.
    const shape = object.shape;
    const middle =
      shape.type === "line" && shape.points.length >= 3
        ? (() => {
            const at = Math.floor(shape.points.length / 3 / 2) * 3;
            return new THREE.Vector3(n(shape.points[at]), n(shape.points[at + 1]), n(shape.points[at + 2]));
          })()
        : undefined;
    const follow = middle
      ? () => node.localToWorld(middle.clone())
      : () => node.getWorldPosition(new THREE.Vector3()).add(new THREE.Vector3(0, lift, 0));
    if (shape.type === "label") {
      anchors.set(`shape:${object.id}`, () => node.getWorldPosition(new THREE.Vector3()));
    } else {
      anchors.set(`object:${object.id}`, follow);
    }
    anchors.set(
      `panel:${object.id}`,
      middle ? follow : () => node.getWorldPosition(new THREE.Vector3()).sub(new THREE.Vector3(0, drop, 0)),
    );
  }

  // Motion: each mover's pose at rest.
  const movers: Mover[] = [];
  for (const motion of scene.motion ?? []) {
    const node = nodes.get(motion.target);
    if (!node) continue;
    movers.push({
      node,
      motion,
      rest: {
        position: [node.position.x, node.position.y, node.position.z],
        rotation: [node.rotation.x, node.rotation.y, node.rotation.z],
        scale: [node.scale.x, node.scale.y, node.scale.z],
      },
    });
  }

  // What this version keeps of the last: objects with the same ids ease to their new places.
  const rest = new Map<string, Remembered>();
  for (const [id, node] of nodes) {
    rest.set(id, {
      position: [node.position.x, node.position.y, node.position.z],
      scale: [node.scale.x, node.scale.y, node.scale.z],
      heights: barMeshes.get(id)?.heights,
    });
  }
  const easing: Easing[] = [];
  if (!reduced && remembered && remembered.version < version) {
    for (const [id, to] of rest) {
      const from = remembered.objects.get(id);
      const node = nodes.get(id);
      if (!from || !node) continue;
      const bars = barMeshes.get(id);
      easing.push({
        node,
        from,
        to,
        bars:
          bars && from.heights && from.heights.length === bars.heights.length
            ? { mesh: bars.mesh, at: bars.at, width: bars.width, from: from.heights, to: bars.heights }
            : undefined,
      });
    }
  }

  // Objects new in this version fade in.
  const appearing: Appearing[] = [];
  if (!reduced && remembered && remembered.version < version) {
    const faded = new Set<THREE.Material>([selected]);
    for (const [id, node] of nodes) {
      if (remembered.objects.has(id)) continue;
      node.traverse((child) => {
        const held = (child as THREE.Mesh).material;
        for (const material of Array.isArray(held) ? held : held ? [held] : []) {
          if (faded.has(material)) continue;
          faded.add(material);
          appearing.push({ material, opacity: material.opacity, transparent: material.transparent });
          material.transparent = true;
          material.opacity = 0;
        }
      });
    }
  }

  return {
    renderer,
    scene: world,
    camera,
    controls,
    nodes,
    anchors,
    pickables,
    picks,
    outlines,
    movers,
    easing,
    appearing,
    home: { position: new THREE.Vector3(...fitted.position), target: new THREE.Vector3(...fitted.target) },
    rest,
    dispose: () => {
      controls.dispose();
      for (const thing of disposables) thing.dispose();
      renderer.dispose();
    },
  };
}

/**
 * Where the axes' names and ticks go, around `bounds`. The camera starts on the +x, +z side, so
 * they go where they read in front.
 */
function axisAnchors(scene: DrawnScene, bounds: THREE.Box3): Map<string, THREE.Vector3> {
  const points = new Map<string, THREE.Vector3>();
  const axes = scene.axes;
  if (bounds.isEmpty()) return points;
  const floor = Math.min(0, bounds.min.y);
  if (axes?.x) points.set("axis:x", new THREE.Vector3(bounds.max.x + 0.6, floor, bounds.max.z + 0.4));
  if (axes?.y) points.set("axis:y", new THREE.Vector3(bounds.min.x - 0.6, bounds.max.y + 0.5, bounds.max.z));
  if (axes?.z) points.set("axis:z", new THREE.Vector3(bounds.max.x + 0.4, floor, bounds.min.z - 0.6));
  (axes?.ticks ?? []).forEach((tick, index) => {
    const at = n(tick.at);
    const point =
      tick.axis === "x"
        ? new THREE.Vector3(at, floor, bounds.max.z + 0.5)
        : tick.axis === "z"
          ? new THREE.Vector3(bounds.max.x + 0.5, floor, at)
          : new THREE.Vector3(bounds.min.x - 0.5, at, bounds.max.z);
    points.set(`tick:${index}`, point);
  });
  return points;
}

/** Draws `bars` at `heights`. */
function placeHeights(bars: NonNullable<Easing["bars"]>, heights: number[]) {
  const matrix = new THREE.Matrix4();
  const rotation = new THREE.Quaternion();
  heights.forEach((height, index) => {
    matrix.compose(
      new THREE.Vector3(bars.at[index * 2], height < 0 ? height : 0, bars.at[index * 2 + 1]),
      rotation,
      new THREE.Vector3(bars.width, Math.max(Math.abs(height), 0.001), bars.width),
    );
    bars.mesh.setMatrixAt(index, matrix);
  });
  bars.mesh.instanceMatrix.needsUpdate = true;
}

/** How long a change of version takes to ease in. */
const EASE_MS = 500;
/** Frames this slow, this many in a row, pause the motion. */
const SLOW_MS = 120;
const SLOW_FRAMES = 30;

export default function SceneCanvas(props: SceneCanvasProps) {
  const { scene, memoryKey, version, picked, pickable, panels, paused, reducedMotion } = props;
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const tagElements = useRef(new Map<string, HTMLElement>());
  const panelElements = useRef(new Map<string, HTMLElement>());
  const worldRef = useRef<World | null>(null);
  const propsRef = useRef(props);
  propsRef.current = props;
  const [theme, setTheme] = useState(0);
  const tags = useMemo(() => tagsOf(scene), [scene]);
  const height = Math.min(960, Math.max(200, scene.height ?? 380));
  const request = useRef<() => void>(() => undefined);

  // The home's theme changed: draw again in its colours.
  useEffect(() => {
    const again = () => setTheme((count) => count + 1);
    const observer = new MutationObserver(again);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "class"] });
    const dark = window.matchMedia?.("(prefers-color-scheme: dark)");
    dark?.addEventListener?.("change", again);
    return () => {
      observer.disconnect();
      dark?.removeEventListener?.("change", again);
    };
  }, []);

  useEffect(() => {
    const element = canvas.current;
    const container = wrap.current;
    if (!element || !container) return;
    let world: World;
    try {
      world = build(scene, element, readPalette(container), memoryKey, version, reducedMotion);
    } catch (error) {
      propsRef.current.onFail(error instanceof Error ? error.message : "3D failed");
      return;
    }
    worldRef.current = world;
    const lost = (event: Event) => {
      event.preventDefault();
      propsRef.current.onFail("the 3D view was lost");
    };
    element.addEventListener("webglcontextlost", lost);

    let frame = 0;
    let shown = true;
    let elapsed = 0;
    let last = performance.now();
    let slow = 0;
    const eased = performance.now();
    const project = new THREE.Vector3();

    const place = () => {
      const width = container.clientWidth;
      const tall = container.clientHeight;
      const position = (key: string, element: HTMLElement | undefined, below: boolean) => {
        const anchor = world.anchors.get(key);
        if (!element || !anchor) return;
        project.copy(anchor()).project(world.camera);
        const inside = project.z < 1 && Math.abs(project.x) <= 1.05 && Math.abs(project.y) <= 1.05;
        element.style.visibility = inside ? "visible" : "hidden";
        if (!inside) return;
        const x = ((project.x + 1) / 2) * width;
        const y = ((1 - project.y) / 2) * tall;
        // A label sits above its object and a panel under it, so neither hides the other; a
        // panel is held inside the stage.
        if (below) {
          const left = Math.min(Math.max(4, x - element.offsetWidth / 2), Math.max(4, width - element.offsetWidth - 4));
          const top = Math.min(y + 8, Math.max(4, tall - element.offsetHeight - 4));
          element.style.transform = `translate(${left.toFixed(1)}px, ${top.toFixed(1)}px)`;
        } else {
          element.style.transform = `translate(${x.toFixed(1)}px, ${y.toFixed(1)}px) translate(-50%, -100%)`;
        }
      };
      for (const [key, element] of tagElements.current) position(key, element, false);
      for (const [key, element] of panelElements.current) position(`panel:${key}`, element, true);
    };

    const tick = (now: number) => {
      frame = 0;
      const current = propsRef.current;
      const dt = Math.min(0.1, (now - last) / 1000);
      last = now;
      const moving = world.movers.length > 0 && !current.paused && !current.reducedMotion && shown;
      if (moving) {
        elapsed += dt;
        for (const mover of world.movers) {
          const pose = motionAt(mover.motion, mover.rest, elapsed) ?? mover.rest;
          mover.node.position.set(...pose.position);
          mover.node.rotation.set(...pose.rotation);
          mover.node.scale.set(...pose.scale);
        }
      }
      let easingNow = false;
      if (world.easing.length > 0 || world.appearing.length > 0) {
        const t = Math.min(1, (now - eased) / EASE_MS);
        for (const fade of world.appearing) fade.material.opacity = tween(0, fade.opacity, t);
        for (const ease of world.easing) {
          ease.node.position.set(...([0, 1, 2].map((k) => tween(ease.from.position[k], ease.to.position[k], t)) as Vec3));
          ease.node.scale.set(...([0, 1, 2].map((k) => tween(ease.from.scale[k], ease.to.scale[k], t)) as Vec3));
          const bars = ease.bars;
          if (bars) placeHeights(bars, bars.to.map((to, index) => tween(bars.from[index], to, t)));
        }
        easingNow = t < 1;
        if (!easingNow) {
          world.easing = [];
          for (const fade of world.appearing) {
            fade.material.opacity = fade.opacity;
            fade.material.transparent = fade.transparent;
            fade.material.needsUpdate = true;
          }
          world.appearing = [];
        }
      }
      world.controls.autoRotate = scene.camera?.spin === true && !current.paused && !current.reducedMotion && shown;
      const turned = world.controls.update();
      world.renderer.render(world.scene, world.camera);
      place();
      const spent = performance.now() - now;
      slow = spent > SLOW_MS ? slow + 1 : 0;
      if (slow > SLOW_FRAMES && moving) {
        slow = 0;
        current.onSlow();
      }
      // The controls' own "change" may already have asked for the next frame: one is enough.
      if ((moving || easingNow || turned || world.controls.autoRotate) && !frame) frame = requestAnimationFrame(tick);
    };
    const again = () => {
      if (!frame) {
        last = performance.now();
        frame = requestAnimationFrame(tick);
      }
    };
    request.current = again;

    const resize = () => {
      const width = Math.max(1, container.clientWidth);
      const tall = Math.max(1, container.clientHeight);
      world.renderer.setSize(width, tall, false);
      world.camera.aspect = width / tall;
      world.camera.updateProjectionMatrix();
      again();
    };
    const sizes = new ResizeObserver(resize);
    sizes.observe(container);
    const seen = new IntersectionObserver(([entry]) => {
      shown = !!entry?.isIntersecting;
      if (shown) again();
    });
    seen.observe(container);
    const hidden = () => {
      shown = document.visibilityState === "visible";
      if (shown) again();
    };
    document.addEventListener("visibilitychange", hidden);
    world.controls.addEventListener("change", again);
    resize();

    // Picking: a click that didn't drag, on an object with a value.
    const ray = new THREE.Raycaster();
    let down: { x: number; y: number } | undefined;
    const hit = (event: PointerEvent): SceneObject | undefined => {
      const box = element.getBoundingClientRect();
      const pointer = new THREE.Vector2(
        ((event.clientX - box.left) / box.width) * 2 - 1,
        -((event.clientY - box.top) / box.height) * 2 + 1,
      );
      ray.setFromCamera(pointer, world.camera);
      const [first] = ray.intersectObjects(world.pickables, false);
      return first ? world.picks.get(first.object) : undefined;
    };
    const pressed = (event: PointerEvent) => {
      down = { x: event.clientX, y: event.clientY };
      propsRef.current.onTouch?.();
    };
    const released = (event: PointerEvent) => {
      const current = propsRef.current;
      if (!down || Math.hypot(event.clientX - down.x, event.clientY - down.y) > 6 || !current.pickable) return;
      const object = hit(event);
      if (object?.value) current.onPick?.(object.value, visible(object.label ?? object.value));
    };
    const hovered = (event: PointerEvent) => {
      if (!propsRef.current.pickable || world.pickables.length === 0) return;
      element.style.cursor = hit(event) ? "pointer" : "";
    };
    element.addEventListener("pointerdown", pressed);
    element.addEventListener("pointerup", released);
    element.addEventListener("pointermove", hovered);

    return () => {
      cancelAnimationFrame(frame);
      sizes.disconnect();
      seen.disconnect();
      document.removeEventListener("visibilitychange", hidden);
      element.removeEventListener("webglcontextlost", lost);
      element.removeEventListener("pointerdown", pressed);
      element.removeEventListener("pointerup", released);
      element.removeEventListener("pointermove", hovered);
      remember(memoryKey, {
        version,
        objects: world.rest,
        camera: {
          position: [world.camera.position.x, world.camera.position.y, world.camera.position.z],
          target: [world.controls.target.x, world.controls.target.y, world.controls.target.z],
        },
      });
      world.dispose();
      worldRef.current = null;
    };
  }, [scene, memoryKey, version, reducedMotion, theme]);

  // What is picked is outlined, in the scene's own colour for it, never the focus colour.
  useEffect(() => {
    const world = worldRef.current;
    if (!world) return;
    const tint = new THREE.Color(readPalette(wrap.current)["--scene-selected"]);
    for (const object of scene.objects) {
      const outline = world.outlines.get(object.id);
      if (!outline) continue;
      const on = isPicked(object.value, picked);
      for (const line of outline.lines) line.visible = on;
      for (const material of outline.materials) {
        material.emissive = on ? tint : new THREE.Color(0x000000);
        material.emissiveIntensity = on ? 0.28 : 0;
      }
    }
    request.current();
  }, [picked, scene, theme, pickable]);

  useEffect(() => {
    request.current();
  }, [paused]);

  // The keyboard turns and zooms the camera; Home puts it back.
  const keyed = (event: KeyboardEvent<HTMLDivElement>) => {
    const world = worldRef.current;
    if (!world || scene.camera?.fixed) return;
    const offset = world.camera.position.clone().sub(world.controls.target);
    const around = new THREE.Spherical().setFromVector3(offset);
    switch (event.key) {
      case "ArrowLeft":
        around.theta -= 10 * DEG;
        break;
      case "ArrowRight":
        around.theta += 10 * DEG;
        break;
      case "ArrowUp":
        around.phi = Math.max(0.1, around.phi - 8 * DEG);
        break;
      case "ArrowDown":
        around.phi = Math.min(Math.PI - 0.1, around.phi + 8 * DEG);
        break;
      case "+":
      case "=":
        around.radius *= 0.85;
        break;
      case "-":
      case "_":
        around.radius /= 0.85;
        break;
      case "Home":
        world.camera.position.copy(world.home.position);
        world.controls.target.copy(world.home.target);
        world.controls.update();
        request.current();
        event.preventDefault();
        return;
      default:
        return;
    }
    event.preventDefault();
    world.camera.position.copy(world.controls.target).add(new THREE.Vector3().setFromSpherical(around));
    world.controls.update();
    request.current();
  };

  return (
    <div
      ref={wrap}
      className="drawn-scene-stage"
      data-scene="drawn"
      style={{ height }}
      tabIndex={scene.camera?.fixed ? -1 : 0}
      onKeyDown={keyed}
    >
      <canvas ref={canvas} className="drawn-scene-canvas" aria-hidden="true" />
      <div className="drawn-scene-overlay" aria-hidden="true">
        {tags.map((tag) => (
          <span
            key={tag.key}
            className={tag.className}
            ref={(element) => {
              if (element) tagElements.current.set(tag.key, element);
              else tagElements.current.delete(tag.key);
            }}
          >
            {tag.text}
          </span>
        ))}
      </div>
      <div className="drawn-scene-panels">
        {panels.map((panel) => (
          <div
            key={panel.key}
            className="drawn-scene-panel"
            ref={(element) => {
              if (element) panelElements.current.set(panel.anchor, element);
              else panelElements.current.delete(panel.anchor);
            }}
          >
            {panel.content}
          </div>
        ))}
      </div>
    </div>
  );
}

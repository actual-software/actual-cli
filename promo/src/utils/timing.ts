import { TIMING } from "../data/brand";

export const fps = () => TIMING.fps;

export const secondsToFrames = (seconds: number): number =>
  Math.round(seconds * TIMING.fps);

export const framesToSeconds = (frames: number): number =>
  frames / TIMING.fps;

export const framesToMs = (frames: number): number =>
  (frames / TIMING.fps) * 1000;

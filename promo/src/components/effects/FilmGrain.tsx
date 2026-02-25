import React, { useRef, useEffect } from "react";
import { useCurrentFrame } from "remotion";

interface FilmGrainProps {
  opacity?: number;
  width: number;
  height: number;
}

export const FilmGrain: React.FC<FilmGrainProps> = ({
  opacity = 0.035,
  width,
  height,
}) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const frame = useCurrentFrame();
  // Redraw every 3 frames (20fps grain at 60fps render)
  const grainFrame = Math.floor(frame / 3);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    // Seed based on grainFrame so it's deterministic per frame (reproducible renders)
    // Simple LCG seeded with grainFrame
    let seed = grainFrame * 1664525 + 1013904223;
    const rand = () => {
      seed = (seed * 1664525 + 1013904223) & 0xffffffff;
      return (seed >>> 0) / 0xffffffff;
    };

    const imageData = ctx.createImageData(width, height);
    const data = imageData.data;
    for (let i = 0; i < data.length; i += 4) {
      const v = Math.floor(rand() * 255);
      data[i] = v;
      data[i + 1] = v;
      data[i + 2] = v;
      data[i + 3] = 255;
    }
    ctx.putImageData(imageData, 0, 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [grainFrame, width, height]);

  return (
    <canvas
      ref={canvasRef}
      width={width}
      height={height}
      style={{
        position: "absolute",
        top: 0,
        left: 0,
        pointerEvents: "none",
        mixBlendMode: "overlay",
        opacity,
      }}
    />
  );
};

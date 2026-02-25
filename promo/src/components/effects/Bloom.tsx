import React from "react";

interface BloomProps {
  /** 0–1 intensity */
  intensity?: number;
  width: number;
  height: number;
}

export const Bloom: React.FC<BloomProps> = ({
  intensity = 0.3,
  width,
  height,
}) => {
  if (intensity <= 0) return null;
  const stdDev = intensity * 12;
  const opacity = intensity * 0.25;

  return (
    <svg
      width={width}
      height={height}
      style={{
        position: "absolute",
        top: 0,
        left: 0,
        pointerEvents: "none",
        mixBlendMode: "screen",
        opacity,
      }}
    >
      <defs>
        <filter id="bloom-filter" x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation={stdDev} result="blur" />
          <feComposite in="blur" in2="SourceGraphic" operator="over" />
        </filter>
      </defs>
      <rect
        width={width}
        height={height}
        fill="none"
        filter="url(#bloom-filter)"
      />
    </svg>
  );
};

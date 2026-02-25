import React from "react";
import { COLORS } from "../../data/brand";

interface GlowWrapperProps {
  children: React.ReactNode;
  color?: string;
  /** 0–1, drives glow radius. Values > 1 are clamped. */
  intensity: number;
  maxRadius?: number;
}

export const GlowWrapper: React.FC<GlowWrapperProps> = ({
  children,
  color = COLORS.borderGreen,
  intensity,
  maxRadius = 20,
}) => {
  const clamped = Math.min(Math.max(intensity, 0), 1.3);
  const radius = clamped * maxRadius;
  return (
    <div
      style={{
        display: "inline-flex",
        filter:
          radius > 0.5
            ? `drop-shadow(0 0 ${radius}px ${color}) drop-shadow(0 0 ${radius * 1.8}px ${color}44)`
            : "none",
      }}
    >
      {children}
    </div>
  );
};

import React from "react";
import { COLORS } from "../../data/brand";

interface TerminalWindowProps {
  children: React.ReactNode;
  width?: number;
  height?: number;
  glowIntensity?: number; // 0–1, drives border glow
}

export const TerminalWindow: React.FC<TerminalWindowProps> = ({
  children,
  width = 1200,
  height = 680,
  glowIntensity = 0,
}) => {
  const glow = glowIntensity * 24;
  return (
    <div
      style={{
        // Gradient border trick: outer div has gradient bg, inner has solid bg with padding gap
        background: `linear-gradient(135deg, ${COLORS.borderGreen}, ${COLORS.borderTeal})`,
        padding: 1.5,
        borderRadius: 12,
        width,
        height,
        boxShadow:
          glow > 0
            ? `0 0 ${glow}px ${COLORS.borderGreen}66, 0 0 ${glow * 2}px ${COLORS.borderGreen}33`
            : "0 20px 60px rgba(0,0,0,0.8)",
        transition: "box-shadow 0.1s",
      }}
    >
      <div
        style={{
          background: COLORS.surface,
          borderRadius: 10.5,
          width: "100%",
          height: "100%",
          overflow: "hidden",
          display: "flex",
          flexDirection: "column",
        }}
      >
        {children}
      </div>
    </div>
  );
};

import React from "react";
import { COLORS, FONTS } from "../data/brand";

// Verification frame: confirms font, colors, and box-drawing chars render correctly
export const TestFrame: React.FC = () => {
  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        fontFamily: FONTS.mono,
        color: COLORS.textPrimary,
      }}
    >
      <pre
        style={{
          fontFamily: FONTS.mono,
          fontSize: 20,
          lineHeight: 1.5,
          color: COLORS.borderGreen,
        }}
      >
        {`╭─────────────────────────╮\n│  actual sync  ✔  done   │\n╰─────────────────────────╯`}
      </pre>
    </div>
  );
};

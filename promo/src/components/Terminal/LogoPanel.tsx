import React from "react";
import { COLORS, FONTS } from "../../data/brand";

// Blank strings at start and end produce one empty line of vertical padding
// above and below the art, matching the real TUI box layout.
const BANNER_LINES = [
  "",
  "       ..===************",
  "     ..****************",
  "   .******      ********+************",
  "  =*****   ****   +*****************",
  "*******   ******   .*** :****",
  "  ****=   ******   **********+++=",
  "   *****    **.    **+ :**+",
  "    *****.         ************",
  "      ***********- *********-",
  "        **********************+",
  "           :-++**********",
  "",
];

function interpolateColor(t: number): string {
  // t: 0 = #00FB7E, 1 = #179CA9
  const r = Math.round(0x00 + t * (0x17 - 0x00));
  const g = Math.round(0xfb + t * (0x9c - 0xfb));
  const b = Math.round(0x7e + t * (0xa9 - 0x7e));
  return `rgb(${r},${g},${b})`;
}

export const LogoPanel: React.FC = () => {
  return (
    // Outer div centers the art block horizontally. No padding here —
    // the blank BANNER_LINES entries provide vertical spacing.
    <div
      style={{
        display: "flex",
        justifyContent: "center",
        padding: "0 8px",
      }}
    >
      {/* Inner div sizes to the widest line; outer flex centers it */}
      <div
        style={{
          fontFamily: FONTS.mono,
          fontSize: 12,
          lineHeight: 1.4,
          color: COLORS.textPrimary,
        }}
      >
        {BANNER_LINES.map((line, lineIdx) => (
          <div key={lineIdx} style={{ whiteSpace: "pre" }}>
            {line.length === 0
              ? // Empty line: render a non-breaking space so the div retains
                // its lineHeight instead of collapsing to zero height.
                "\u00A0"
              : line.split("").map((char, charIdx) => {
                  const maxWidth = 44; // left panel character width
                  const t = charIdx / maxWidth;
                  return (
                    <span key={charIdx} style={{ color: interpolateColor(t) }}>
                      {char}
                    </span>
                  );
                })}
          </div>
        ))}
      </div>
    </div>
  );
};

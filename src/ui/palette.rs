//! Official Attacca dark-mode color palette (from attacca-web/src/index.css).

use ratatui::style::Color;

pub const BG: Color = Color::Rgb(20, 16, 14);          // --background:  #14100E
pub const CARD: Color = Color::Rgb(29, 23, 21);        // --card:        #1D1715
pub const POPOVER: Color = Color::Rgb(37, 30, 27);     // --popover:     #251E1B

pub const TEXT: Color = Color::Rgb(236, 231, 229);     // --foreground:  #ECE7E5
pub const DIM: Color = Color::Rgb(158, 142, 137);      // --muted-foreground: #9E8E89

pub const P: Color = Color::Rgb(204, 109, 92);          // --primary:     #CC6D5C
pub const P_FG: Color = Color::Rgb(13, 9, 8);           // --primary-foreground: #0D0908
pub const P_DIM: Color = Color::Rgb(180, 85, 70);

pub const ACCENT_BG: Color = Color::Rgb(62, 39, 35);    // --accent:      #3E2723

pub const DESTRUCTIVE: Color = Color::Rgb(232, 88, 84); // --destructive: #E85854

// border: #FFFDF9 @ 13 % over #14100E ≈ #2D2A27
pub const BORDER: Color = Color::Rgb(45, 42, 39);

pub const GREEN: Color = Color::Rgb(90, 180, 115);
pub const YELLOW: Color = Color::Rgb(220, 175, 65);

pub const SIDEW: u16 = 28;

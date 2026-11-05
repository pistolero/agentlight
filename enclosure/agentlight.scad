// Agentlight enclosure for Waveshare ESP32-S3-Touch-LCD-5B.
// Units: mm. Preview with part="assembly"; export one part at a time.
// Measure the actual board and USB connector before final printing.

part = "assembly"; // [assembly, housing, cover, stand, fit_test, interference, screen_interference]
screen_angle = 45; // [25:5:75] angle from horizontal, fixed when stand is printed
vent_holes = true;

// Waveshare touch-version drawing: glass 127.70 x 87.45, active area
// 109.00 x 65.80; total stack approximately 14 mm. Verify actual board.
glass_width = 127.70;
glass_height = 87.45;
visible_width = 109.00;
visible_height = 65.80;
fit_clearance = 0.50;      // total clearance per dimension, not per side
wall = 2.4;
bezel_thickness = 2.0;
case_depth = 20.0;        // housing front to rear cover, excluding cover thickness
cover_thickness = 2.4;
ear_width = 7.0;
screw_diameter = 2.2;     // pilot holes for M2.5 self-tapping screws; calibrate
screw_clearance = 2.9;
fastener_edge_offset = 12;

// USB opening: looking at the SCREEN from the front, the reference drawing
// places USB on the right. Check your board/cable before committing to a print.
usb_side = "right";       // [right,left]
usb_center_y = 37.5;       // from bottom edge of glass; provisional
usb_center_z = 10.0;      // behind front of housing; provisional
usb_opening_length = 18.0; // along the edge; plug housing, not just socket
usb_opening_depth = 12.0;

stand_clearance = 0.8;    // total extra slot width/depth
stand_wall = 4.0;
stand_insertion = 10.0;   // along housing height, below the visible window
stand_width = glass_width + fit_clearance + 2*wall + 2*ear_width + 2*stand_wall + 1;
stand_base_depth = 100.0;
stand_base_thickness = 5.0;
stand_front_offset = 10.0;
stand_block_height = 29.0;

$fn = 32;
inner_w = glass_width + fit_clearance;
inner_h = glass_height + fit_clearance;
outer_w = inner_w + 2*wall;
outer_h = inner_h + 2*wall;
full_depth = case_depth + cover_thickness;
eps = 0.05;
hole_x = outer_w/2 + ear_width/2;
hole_y = outer_h/2 - fastener_edge_offset;

assert(screen_angle >= 25 && screen_angle <= 75, "screen_angle must be 25..75 degrees");
assert(visible_width < glass_width && visible_height < glass_height, "window must be smaller than glass");
assert(case_depth > bezel_thickness + 14, "allow room for the display stack");
slot_width = outer_w + 2*ear_width + stand_clearance;
assert(stand_width >= slot_width + 2*stand_wall, "stand must retain both sides and clear the cover mounting ears");
assert(stand_insertion < (outer_h-visible_height-1)/2, "stand slot must not cover the active screen");
assert(stand_base_depth > stand_front_offset + full_depth*sin(screen_angle) + 20, "increase stand_base_depth");

// Local case coordinates: X across display, Y from bottom to top, Z from
// front to rear. Housing is printed front-down, cover rear-down separately.
module mounting_points() {
    for (x=[-hole_x,hole_x], y=[-hole_y,hole_y]) translate([x,y,0]) children();
}

module housing() {
    difference() {
        union() {
            translate([-outer_w/2,-outer_h/2,0]) cube([outer_w,outer_h,case_depth]);
            mounting_points() translate([-ear_width/2,-5,0]) cube([ear_width,10,case_depth]);
        }
        // Window exposes the entire active LCD area, with a little margin.
        translate([-(visible_width+1)/2,-(visible_height+1)/2,-eps])
            cube([visible_width+1,visible_height+1,bezel_thickness+2*eps]);
        // Insert from rear: do not cut away the front bezel.
        translate([-inner_w/2,-inner_h/2,bezel_thickness])
            cube([inner_w,inner_h,case_depth+eps]);
        // Cut fully through the selected side wall; opens to rear for inserting
        // the board even if its connector projects past the PCB edge.
        translate([usb_side=="right" ? inner_w/2-eps : -outer_w/2-eps,
                   -glass_height/2+usb_center_y-usb_opening_length/2,
                   usb_center_z-usb_opening_depth/2])
            cube([wall+2*eps,usb_opening_length,case_depth+eps]);
        mounting_points() translate([0,0,case_depth-10]) cylinder(h=10+eps,d=screw_diameter);
    }
}

module cover() {
    difference() {
        union() {
            translate([-outer_w/2,-outer_h/2,0]) cube([outer_w,outer_h,cover_thickness]);
            mounting_points() translate([-ear_width/2,-5,0]) cube([ear_width,10,cover_thickness]);
        }
        mounting_points() translate([0,0,-eps]) cylinder(h=cover_thickness+2*eps,d=screw_clearance);
        // Vents deliberately stay away from the mounting ears and cover edges.
        if (vent_holes)
            for (x=[-45:9:45], y=[-24:8:24])
                translate([x,y,-eps]) hull() {
                    translate([0,-2,0]) cylinder(h=cover_thickness+2*eps,d=3);
                    translate([0, 2,0]) cylinder(h=cover_thickness+2*eps,d=3);
                }
    }
}

// Place case upright in the stand. Rear points down/back relative to front.
// The printed slot is wider and deeper than the housing by stand_clearance.
stand_z = stand_base_thickness + full_depth*cos(screen_angle) + 1;
// Keep the cradle's top below the lower edge of the visible screen, including
// at steep angles. The two-millimetre margin is measured along the case face.
cradle_height = min(stand_block_height, stand_z + ((outer_h-visible_height-1)/2 - 2)*sin(screen_angle));
module in_stand() {
    translate([0,stand_front_offset,stand_z])
        rotate([screen_angle,0,0]) mirror([0,0,1])
            translate([0,outer_h/2,0]) children();
}

module stand() {
    difference() {
        union() {
            translate([-stand_width/2,0,0]) cube([stand_width,stand_base_depth,stand_base_thickness]);
            // Low cradle: supporting the lower edge leaves the USB area clear.
            translate([-stand_width/2,0,0]) cube([stand_width,43,cradle_height]);
        }
        // Top-entry slot: keep the mounting ears above the stand and do not
        // clamp the glass. The slot intersects the top so insertion is possible.
        in_stand() translate([-slot_width/2,-outer_h/2-eps,-stand_clearance/2])
            cube([slot_width,stand_insertion+eps,full_depth+stand_clearance]);
        // Remove everything above the slot opening to provide an insertion path.
        in_stand() translate([-slot_width/2,-outer_h/2+stand_insertion,-stand_clearance/2])
            cube([slot_width,outer_h+stand_base_depth,full_depth+stand_clearance]);
        // Space behind the cradle lets the enclosure lean, without adding a
        // solid wall across the rear USB opening.
    }
}

// Quickly test fit on a printer without producing a full enclosure.
module fit_test() {
    intersection() {
        housing();
        translate([outer_w/2-14,-outer_h/2-1,-1]) cube([ear_width+15,27,case_depth+2]);
    }
}

if (part=="housing") housing();
else if (part=="cover") cover();
else if (part=="stand") stand();
else if (part=="fit_test") fit_test();
// An empty intersection confirms the installed housing/cover fit the cradle.
else if (part=="interference") intersection() {
    stand();
    in_stand() union() { housing(); translate([0,0,case_depth]) cover(); }
}
else if (part=="screen_interference") intersection() {
    stand();
    in_stand() translate([-visible_width/2,-visible_height/2,-0.1]) cube([visible_width,visible_height,0.2]);
}
else if (part=="assembly") {
    color("#3c4655") stand();
    in_stand() {
        color("#202934") housing();
        translate([0,0,case_depth]) color("#586779") cover();
        // Reference only: translucent nominal glass and board envelope.
        color([0.1,0.6,0.7,0.35]) translate([-glass_width/2,-glass_height/2,bezel_thickness]) cube([glass_width,glass_height,14]);
    }
}

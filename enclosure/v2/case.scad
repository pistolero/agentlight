// Waveshare touch LCD 5B: all units mm. See README before assembly.
part="assembly"; // [assembly, shell, bezel, stand, exploded, collision]
angle=45; // [30:5:65] fixed printed angle, from desk
vents=true;
clearance=0.6;
glass=[127.7,87.45];
pcb=[120.5,75.5];
mount_spacing=[111,63];
// Front-to-rear mounting-face distance from touch-version side view.
// Adjust if the mounting faces on your physical revision differ.
mount_to_glass=14;
floor_t=2.4;
post_h=6;
glass_gap=0.6;
bezel_t=2.4;
wall=2.4;
size=[145,105];
rim=floor_t+post_h+mount_to_glass+glass_gap;
depth=rim+bezel_t;
$fn=40;
eps=0.05;

assert(angle>=30 && angle<=65);
assert(mount_to_glass>=12 && mount_to_glass<=16);

module outline(w,h,r=4) {
    hull() for(x=[-w/2+r,w/2-r],y=[-h/2+r,h/2-r]) translate([x,y]) circle(r=r);
}
module block(w,h,d,r=4) { linear_extrude(d) outline(w,h,r); }
module board_mounts() {
    // PCB is centered horizontally. Touch glass extends unequally vertically:
    // the active area is 9.35 mm from the top edge. PCB offset remains a
    // revision-dependent fit parameter; drawing's outer outlines imply -0.225.
    for(x=[-55.5,55.5],y=[-31.725,31.275]) translate([x,y,0]) children();
}
module lid_mounts() {
    for(x=[-67,67],y=[-47,47]) translate([x,y,0]) children();
}
module shell() {
    difference() {
        union() {
            difference() {
                block(size[0],size[1],rim);
                translate([0,0,floor_t]) block(size[0]-2*wall,size[1]-2*wall,rim+eps,2);
            }
            board_mounts() cylinder(h=floor_t+post_h,d=6);
            // Four corner towers tied to the side walls. Outside the glass.
            lid_mounts() cylinder(h=rim,d=9);
        }
        board_mounts() translate([0,0,-eps]) cylinder(h=floor_t+post_h+2*eps,d=2.9);
        lid_mounts() translate([0,0,rim-10]) cylinder(h=10+eps,d=2.5);
        // Right-hand USB access, seen from screen/front. Generous opening also
        // exposes BOOT/RESET. It reaches the rim: no unsupported top bridge.
        translate([size[0]/2-wall-eps,-17,6]) cube([wall+2*eps,24,rim]);
        if(vents) for(x=[-42:12:42],y=[-31:10:31])
            translate([x,y,-eps]) cylinder(h=floor_t+2*eps,d=3);
    }
}
module bezel() {
    difference() {
        block(size[0],size[1],bezel_t);
        // Touch drawing: active area starts 9.35 from left/top, so its center
        // is (0,1.475) relative to the glass center, screen-front view.
        translate([0,1.475,-eps]) block(111,67.8,bezel_t+2*eps,1);
        lid_mounts() translate([0,0,-eps]) cylinder(h=bezel_t+2*eps,d=3.2);
    }
}

// Stand: broad rear support and two sloped side cheeks. Print base-down.
// A surface that rises at angle >=30deg is a <=60deg overhang; the cheeks
// provide continuous support, not a bridged sloping roof.
base_d=105;
base_w=151;
front_y=9;
bottom_z=10;
module seated() {
    // Case local rear is z=0. Face points forward/up; bottom sits in cradle.
    translate([0,front_y,bottom_z]) rotate([angle,0,0])
        translate([0,size[1]/2,0]) children();
}
module cheek() {
    // Lower portion of a triangle, enough to support housing at two sides.
    linear_extrude(5) polygon([[0,0],[82,0],[82,55],[50,55]]);
}
module stand() {
    difference() {
        union() {
            translate([0,base_d/2,0]) block(base_w,base_d,5);
            // Side wedges: solid underneath, clipped by enclosure below.
            for(x=[-size[0]/2-2.5,size[0]/2-2.5])
                translate([x,front_y,5]) rotate([90,0,90])
                    linear_extrude(5) polygon([[0,0],[82,0],[82,55],[50,55]]);
            // Low front retaining lip, beneath visible screen.
            translate([-base_w/2+3,front_y-5,5]) cube([base_w-6,9,7]);
        }
        // A small clearance prevents interference and allows lifting out.
        seated() translate([0,0,-0.4]) block(size[0]+clearance,size[1]+clearance,depth+0.8,4);
        // Open the insertion path upwards along the face; no snap fit/glass load.
        seated() translate([0,size[1],-0.4]) block(size[0]+clearance,size[1]*2,depth+0.8,4);
    }
}
module reference_board() {
    color("#1f5d64") translate([0,-0.225,floor_t+post_h]) block(pcb[0],pcb[1],1.5,2);
    color("#17212b") translate([0,0,floor_t+post_h+mount_to_glass-2.95]) block(glass[0],glass[1],2.95,4);
    color("#264653") translate([0,1.475,floor_t+post_h+mount_to_glass+0.02]) block(109,65.8,0.1,1);
}
module case_assembly(explode=0) {
    color("#445263") shell();
    reference_board();
    translate([0,0,rim+explode]) color("#202a35") bezel();
}
if(part=="shell") shell();
else if(part=="bezel") bezel();
else if(part=="stand") stand();
else if(part=="exploded") case_assembly(30);
else if(part=="collision") intersection() {
    stand(); seated() union() {shell();translate([0,0,rim]) bezel();}
}
else {
    color("#303b47") stand();
    seated() case_assembly();
}

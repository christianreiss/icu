struct Params {
    // canvas size, tile center
    screen: vec4<f32>,
    // face radius, outer radius, label inner/outer radii
    radius: vec4<f32>,
    // time, intensity, dim multiplier, label sweep
    effect: vec4<f32>,
    color: vec4<f32>,
    // label texture origin and size
    label: vec4<f32>,
    // video width/height, face kind (0 missing, 1 camera, 2 plate), halo width
    image: vec4<f32>,
};
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var video: texture_2d<f32>;
@group(0) @binding(2) var lettering: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let q = array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
    return vec4(q[i],0.,1.);
}
fn hash(q: vec3<f32>) -> f32 { return fract(sin(dot(q,vec3(127.1,311.7,74.7)))*43758.5453); }
fn noise(q: vec3<f32>) -> f32 {
    let i=floor(q); let f=fract(q); let u=f*f*(3.-2.*f);
    return mix(mix(mix(hash(i),hash(i+vec3(1.,0.,0.)),u.x),mix(hash(i+vec3(0.,1.,0.)),hash(i+vec3(1.,1.,0.)),u.x),u.y),mix(mix(hash(i+vec3(0.,0.,1.)),hash(i+vec3(1.,0.,1.)),u.x),mix(hash(i+vec3(0.,1.,1.)),hash(i+vec3(1.,1.,1.)),u.x),u.y),u.z);
}
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let d=pos.xy-p.screen.zw; let r=length(d); let t=p.effect.x;
    let edge=r-p.radius.y;
    var out=vec4(0.);
    if edge > -1. && edge < p.image.w && p.effect.y > 0. {
        // Domain-warped 3D noise makes individual curls move outward and
        // dissolve. Angular variation breaks the silhouette into smoke tongues.
        let q=vec3(d/max(p.radius.x,1.)*4.,t*.26);
        let warp=vec3(noise(q*1.3),noise(q*1.3+vec3(7.)),noise(q+vec3(19.)));
        let n=noise(q+warp*2.4-vec3(0.,t*.36,0.));
        let detail=noise(q*3.8+warp-vec3(0.,t*.8,0.));
        let extent=p.image.w*(.24+.76*n);
        let envelope=1.-smoothstep(0.,extent,edge);
        let strands=smoothstep(.18,.8,n*.65+detail*.55);
        let a=clamp(envelope*strands*p.effect.y*.92,0.,.9);
        let tint=mix(p.color.rgb,vec3(.72,.54,1.),n*.5);
        out=vec4(tint*a,a);
    }
    if r <= p.radius.y {
        var c=vec3(.188);
        if r <= p.radius.x {
            c=vec3(.125);
            if p.image.z > 1.5 { c=vec3(.11); }
            else if p.image.z > .5 {
                let side=min(p.image.x,p.image.y);
                let uv=clamp((d/p.radius.x+1.)*.5,vec2(0.),vec2(.9999));
                let xy=vec2<i32>((p.image.xy-vec2(side))*.5+uv*side);
                c=textureLoad(video,xy,0).rgb;
            }
            c*=p.effect.z;
        }
        out=vec4(c,1.);
    }
    let theta=atan2(d.x,d.y); let mid=(p.radius.z+p.radius.w)*.5; let half=(p.radius.w-p.radius.z)*.5;
    let cap=select(vec2(sin(p.effect.w),cos(p.effect.w))*mid,vec2(0.,mid),theta<0.);
    if p.effect.w>0. && r>p.radius.z && r<=p.radius.w && ((theta>=0. && theta<=p.effect.w) || distance(d,cap)<=half) {
        out=vec4(vec3(242.,232.,208.)/255.*p.effect.z,1.);
    }
    let lp=vec2<i32>(floor(pos.xy-p.label.xy));
    if all(lp>=vec2(0)) && all(lp<vec2<i32>(p.label.zw)) {
        let ink=textureLoad(lettering,lp,0);
        out=ink+out*(1.-ink.a);
    }
    return out;
}

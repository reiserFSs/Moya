override operation: u32;
@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
@group(0) @binding(2) var mask: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba32float, write>;
struct Params { head:vec4<f32>, p:array<vec4<f32>,7> }
@group(0) @binding(4) var<uniform> u:Params;
@group(0) @binding(5) var<storage,read> stops:array<vec4<f32>>;
fn arg(i:u32)->f32{return u.p[i/4u][i%4u];}
fn load(t:texture_2d<f32>,p:vec2<i32>)->vec4<f32>{return textureLoad(t,clamp(p,vec2(0),vec2<i32>(textureDimensions(t))-1),0);}
// Region pointwise inputs may already be cropped; resamplers always read their source grid.
fn region_load(t:texture_2d<f32>,p:vec2<i32>)->vec4<f32>{
 var q=p;
 if(arg(24)>0. && all(textureDimensions(t)==vec2<u32>(u32(arg(24)),u32(arg(25))))){q+=vec2<i32>(i32(arg(26)),i32(arg(27)));}
 return load(t,q);
}
fn linear(t:texture_2d<f32>,p:vec2<f32>)->vec4<f32>{let v=vec2<i32>(floor(p));let f=fract(p);return mix(mix(load(t,v),load(t,v+vec2(1,0)),f.x),mix(load(t,v+vec2(0,1)),load(t,v+vec2(1,1)),f.x),f.y);}
fn cubic(x:f32)->f32{let v=abs(x);if(v<1.){return (1.5*v-2.5)*v*v+1.;}if(v<2.){return ((-.5*v+2.5)*v-4.)*v+2.;}return 0.;}
fn bicubic(p:vec2<f32>)->vec4<f32>{let origin=vec2<i32>(floor(p));var sum=vec4(0.);for(var y=-1;y<=2;y++){for(var x=-1;x<=2;x++){let q=origin+vec2(x,y);sum+=load(a,q)*cubic(p.x-f32(q.x))*cubic(p.y-f32(q.y));}}return sum;}
fn motion_cubic(x:f32)->f32{let v=abs(x);if(v<1.){return (1.25*v-2.25)*v*v+1.;}if(v<2.){return ((-.75*v+3.75)*v-6.)*v+3.;}return 0.;}
fn motion_load(p:vec2<i32>)->vec4<f32>{
 let value=load(a,p);
 return select(value,vec4(0.),(bitcast<vec4<u32>>(value)&vec4(0x7f800000u))==vec4(0x7f800000u));
}
// Inclusive prefix differences plus repeated clamped endpoints, in shear space.
fn motion_box(major:i32,minor:i32,vertical:bool)->vec4<f32>{
 let dims=vec2<i32>(textureDimensions(a));let last=select(dims.x,dims.y,vertical)-1;
 let span=i32(arg(4));let lo=major-span/2;let hi=lo+span;
 var sum=load(a,select(vec2(min(hi,last),minor),vec2(minor,min(hi,last)),vertical));
 if(lo>0){sum-=load(a,select(vec2(lo-1,minor),vec2(minor,lo-1),vertical));}
 if(lo<0){sum+=f32(-lo)*load(a,select(vec2(0,minor),vec2(minor,0),vertical));}
 if(hi>last){
  var endpoint=load(a,select(vec2(last,minor),vec2(minor,last),vertical));
  if(last>0){endpoint-=load(a,select(vec2(last-1,minor),vec2(minor,last-1),vertical));}
  sum+=f32(hi-last)*endpoint;
 }
 var average=sum/f32(span+1);
 if(arg(3)>0.){average=floor(clamp(average,vec4(0.),vec4(1.))*arg(3)+.5)/arg(3);}
 return average;
}
fn decode(c:f32)->f32{if(c<=.04045){return c/12.92;}return pow((c+.055)/1.055,2.4);}
fn encode(c:f32)->f32{if(c<=.0031308){return c*12.92;}return 1.055*pow(c,1./2.4)-.055;}
// PS's embedded sRGB ICC TRC: 1024 unsigned-16 samples, not analytic sRGB.
fn icc_srgb_point(i:f32)->f32{return floor(decode(i/1023.)*65535.+.5)/65535.;}
fn icc_srgb_decode(c:f32)->f32{let x=clamp(c,0.,1.)*1023.;let i=min(floor(x),1022.);return mix(icc_srgb_point(i),icc_srgb_point(i+1.),x-i);}
fn icc_srgb_encode(c:f32)->f32{
 let x=clamp(c,0.,1.);var i=clamp(floor(encode(x)*1023.),0.,1022.);
 var lo=icc_srgb_point(i);var hi=icc_srgb_point(i+1.);
 // Quantized TRC differs from analytic sRGB by less than one sample interval.
 if(x<lo&&i>0.){i-=1.;hi=lo;lo=icc_srgb_point(i);}
 else if(x>hi&&i<1022.){i+=1.;lo=hi;hi=icc_srgb_point(i+1.);}
 return (i+(x-lo)/(hi-lo))/1023.;
}
fn lum(c:vec3<f32>)->f32{return dot(c,vec3(.30,.59,.11));}
// Measured Color-mode integer luminance: fixed 15-bit weights, rounded before gamut compression.
fn colorlum(c:vec3<f32>,grid:f32)->f32{if(grid==0.){return lum(c);}let v=vec3<i32>(floor(c*grid+.5));let l=(v.r*9830+v.g*19334+v.b*3604+16384)/32768;return f32(l)/grid;}
fn clipcolor(c:vec3<f32>,l:f32,grid:f32)->vec3<f32>{let level=i32(round(l*grid));let limit=i32(grid);var r=vec3<i32>(round(c*grid));let n=min(r.r,min(r.g,r.b));let x=max(r.r,max(r.g,r.b));if(n<0){let d=level-n;r=vec3(level)+((r-vec3(level))*level+vec3(d/2))/d;}if(x>limit){let d=x-level;r=vec3(level)+((r-vec3(level))*(limit-level)+vec3(d/2))/d;}return vec3<f32>(r)/grid;}
// PS 27.11 rejects Screen/Color Dodge layer modes in Float; HDR callers use source-defined depth detours.
fn blendfn(base:vec3<f32>,src:vec3<f32>,mode:u32,grid:f32)->vec3<f32>{
 var o=src;
 if(mode==1u){o=1.-(1.-base)*(1.-src);}
 if(mode==2u){o=base+src;}
 if(mode==3u){for(var i=0u;i<3u;i++){if(base[i]==0.){o[i]=0.;}else if(src[i]>=1.){o[i]=1.;}else{o[i]=min(1.,base[i]/(1.-src[i]));}}}
 if(mode==4u){o=base*src;}
 if(mode==5u){o=max(base,src);}
 if(mode==6u){o=select(1.-2.*(1.-base)*(1.-src),2.*base*src,base<=vec3(.5));}
 if(mode==7u){for(var i=0u;i<3u;i++){let v=base[i];var d=sqrt(max(v,0.));if(v<=.25){d=((16.*v-12.)*v+4.)*v;}o[i]=select(v+(2.*src[i]-1.)*(d-v),v-(1.-2.*src[i])*v*(1.-v),src[i]<=.5);}}
 if(mode==8u){for(var i=0u;i<3u;i++){o[i]=select(base[i]/max(src[i],1e-20),1.,src[i]==0.);}}
 if(mode==9u){let l=colorlum(base,grid);o=src+vec3(l-colorlum(src,grid));if(grid>0.){o=clipcolor(o,l,grid);}}
 if(grid>0.){o=clamp(o,vec3(0.),vec3(1.));}
 return o;
}
fn hsl(c:vec3<f32>)->vec3<f32>{let m=min(c.r,min(c.g,c.b));let v=max(c.r,max(c.g,c.b));let d=v-m;let l=(v+m)*.5;var h=0.;var s=0.;if(d>1e-10){s=d/max(1.-abs(2.*l-1.),1e-10);if(v==c.r){h=(c.g-c.b)/d;}else if(v==c.g){h=(c.b-c.r)/d+2.;}else{h=(c.r-c.g)/d+4.;}h=fract(h/6.);}return vec3(h,s,l);}
fn fromhsl(c:vec3<f32>)->vec3<f32>{let chroma=(1.-abs(2.*c.z-1.))*c.y;let h=fract(c.x)*6.;let x=chroma*(1.-abs((h%2.)-1.));var v=vec3(chroma,x,0.);if(h>=1.){v=vec3(x,chroma,0.);}if(h>=2.){v=vec3(0.,chroma,x);}if(h>=3.){v=vec3(0.,x,chroma);}if(h>=4.){v=vec3(x,0.,chroma);}if(h>=5.){v=vec3(chroma,0.,x);}return v+vec3(c.z-chroma*.5);}
fn satmap(s:f32,amount:f32)->f32{if(amount<=0.){return s*(1.+amount);}if(amount>=1.){return select(0.,1.,s>0.);}return min(1.,s/(1.-amount));}
fn hash(v:u32)->u32{var x=v;x=(x^(x>>16u))*0x7feb352du;x=(x^(x>>15u))*0x846ca68bu;return x^(x>>16u);}
fn random(v:u32)->f32{return f32(hash(v)&0x00ffffffu)/16777216.;}
@compute @workgroup_size(16,16)
fn main(@builtin(global_invocation_id) id:vec3<u32>){let local_size=textureDimensions(dst);if(any(id.xy>=local_size)){return;}let region=arg(24)>0.;let size=select(local_size,vec2<u32>(u32(arg(24)),u32(arg(25))),region);let p=vec2<i32>(id.xy);let fp=vec2<f32>(id.xy)+select(vec2(0.),vec2(arg(26),arg(27)),region);let op=operation;let source=vec2<f32>(textureDimensions(a));var c=region_load(a,p);var out=c;
switch op {
case 0u:{}
case 1u:{out=vec4(arg(0),arg(1),arg(2),arg(3));}
case 2u:{out=linear(a,(fp+.5)*source/vec2<f32>(size)-.5);}
case 3u:{// CALIBRATE: Catmull-Rom bicubic, area prefilter for reduction.
 let ratio=source/vec2<f32>(size);if(any(ratio>vec2(1.))){var sum=vec4(0.);let q=(fp+.5)*ratio-.5;for(var y=0;y<4;y++){for(var x=0;x<4;x++){sum+=linear(a,q+(vec2(f32(x),f32(y))+.5-2.)*ratio/4.);}}out=sum/16.;}else{out=bicubic((fp+.5)*ratio-.5);}}
case 4u:{let center=vec2(arg(0),arg(1))-.5;let q=center+(fp-center)/arg(2);if(any(q<vec2(-.5))||any(q>=source-.5)){out=vec4(0.);}else{out=bicubic(q);}}
case 5u:{out=load(a,p-vec2<i32>(i32(arg(0)),i32(arg(1))));}
case 6u:{
 let ratio=max(source-1.,vec2(0.))/max(vec2<f32>(size)-1.,vec2(1.));
 let q=fp*ratio;
 out=linear(a,q);
 if(arg(0)>0.){
   // Preserve endpoint values: clamped extension must retain original boundary colors.
   let radius=select(ratio*.25,vec2(0.),(id.xy==vec2(0u))|(id.xy+1u==size));
   out=(linear(a,q-radius)+linear(a,q+radius)+linear(a,q+vec2(radius.x,-radius.y))+linear(a,q+vec2(-radius.x,radius.y)))*.25;
 }
}
case 7u:{let r=i32(arg(0));let dir=vec2<i32>(i32(arg(1)),i32(arg(2)));var sum=c*stops[0].x;for(var k=1;k<=r;k++){sum+=(load(a,p+k*dir)+load(a,p-k*dir))*stops[u32(k)].x;}out=sum;}
case 8u:{
 let n=u32(arg(3));var sum=vec4(0.);
 if(arg(4)==2.){
  // Integer Photoshop shears use cubic a=-.75 and clip between passes.
  let vertical=arg(1)>0.;let major=select(p.x,p.y,vertical);let minor=select(p.y,p.x,vertical);
  let g=fract(ceil((f32(major)+.5)*arg(0)*128.)/128.);
  for(var k=-1;k<=2;k++){let q=select(vec2(major,minor-2-k),vec2(minor-2-k,major),vertical);sum+=load(a,q)*motion_cubic(f32(k)-g);}
  out=floor(clamp(sum,vec4(0.),vec4(1.))*arg(6)+.5)/arg(6);
 }else{
  // Inclusive major-axis box between two 1/128-pixel shears.
  // CALIBRATE: nonintegral phase residual, Float corpus max .003692.
  let vertical=arg(1)>0.;let major=select(p.x,p.y,vertical);let minor=select(p.y,p.x,vertical);
  let limit=select(i32(source.x),i32(source.y),vertical)-1;
  let phase=floor((f32(major)+.5)*arg(0)*128.)/128.;let f=fract(phase);
  if(arg(5)>0.){
   for(var j=-1;j<=2;j++){
    var box=vec4(0.);
    for(var k=0u;k<n;k++){
     let q=clamp(major+i32(k)-i32(arg(2))/2,0,limit);
     let other=ceil((f32(q)+.5)*arg(0)*128.)/128.;
     let base=minor+i32(floor(phase)-floor(other));
     let at=select(vec2(q,base+j+2),vec2(base+j+2,q),vertical);box+=load(a,at);
    }
    sum+=floor(box/f32(n)*arg(6)+.5)/arg(6)*motion_cubic(f32(j)-f);
   }
  }else{
  for(var k=0u;k<n;k++){
   let q=clamp(major+i32(k)-i32(arg(2)) / 2,0,limit);
   let other=ceil((f32(q)+.5)*arg(0)*128.)/128.;let g=fract(other);
   let base=minor+i32(floor(phase)-floor(other));
    let lo=select(vec2(q,base-1),vec2(base-1,q),vertical);
    let mid=select(vec2(q,base),vec2(base,q),vertical);
    let hi=select(vec2(q,base+1),vec2(base+1,q),vertical);
    sum+=load(a,lo)*((1.-f)*g)+load(a,mid)*((1.-f)*(1.-g)+f*g)+load(a,hi)*(f*(1.-g));
  }
  }
 out=select(sum/f32(n),clamp(sum,vec4(0.),vec4(1.)),arg(5)>0.);
 }}
case 9u:{// PS 27.11 support is symmetric, +/- amount/400 of radial distance.
 // PS repeats differ; use the uniform sampling expectation, not invented random seeds.
 // CALIBRATE: sparse spatial sample phases remain unmatched; Float impulse max .233837.
 let center=vec2(arg(0),arg(1));let n=u32(arg(3));var sum=vec4(0.);for(var k=0u;k<n;k++){let t=((f32(k)+.5)/f32(n)-.5)*.5;sum+=linear(a,center+(fp-center)*(1.+arg(2)*t));}out=sum/f32(n);}
case 10u:{
 if(arg(1)>8.){
  // PS 27.11 large: deterministic five-plane-wave fit, period 99 pixels.
  // Residual displacement is <0.12 px at amount999; see ps-op-math.md.
  let frequency=array<vec2<f32>,5>(vec2(1.,-2.),vec2(2.,-2.),vec2(3.,-6.),vec2(0.,2.),vec2(3.,-2.));
  let sine=array<vec2<f32>,5>(vec2(-1.85239733,-.74044822),vec2(1.72207367,-1.73872899),vec2(-.91559535,1.84798225),vec2(.00311832,.73529035),vec2(-.78852142,-.83165139));
  let cosine=array<vec2<f32>,5>(vec2(.86973454,1.90775758),vec2(-.54525498,.49029434),vec2(1.65051777,-.39010454),vec2(.73542606,-.01479289),vec2(.40380319,-.30525573));
  var displacement=vec2(0.);
  for(var i=0u;i<5u;i++){
   let phase=vec2(dot(fp,frequency[i]),dot(fp.yx,frequency[i]))*6.283185307/arg(1);
   displacement+=sin(phase)*sine[i]+cos(phase)*cosine[i];
  }
  out=linear(a,fp+displacement*arg(0)/100.);
 }else{
  // CALIBRATE: small-size ripple remains an unmeasured sinusoidal approximation.
  let phase=random(bitcast<u32>(arg(2)))*6.2831853;let wave=vec2(sin(fp.y*6.2831853/arg(1)+phase),sin(fp.x*6.2831853/arg(1)+phase));out=linear(a,fp+wave*arg(0)*arg(1)/100.);
 }}
case 11u:{
 // Measured lens-blur response: constant-preserving radial lobes, not Gaussian USM.
 var detail=vec3(0.);
 for(var y=-3;y<=3;y++){for(var x=-3;x<=3;x++){
  let d=x*x+y*y;var k=9u;
  switch(d){case 1:{k=0u;}case 2:{k=1u;}case 4:{k=2u;}case 5:{k=3u;}case 8:{k=4u;}case 9:{k=5u;}case 10:{k=6u;}case 13:{k=7u;}case 18:{k=8u;}default:{}}
  if(k<9u){detail+=(load(a,p+vec2<i32>(x,y)).rgb-c.rgb)*arg(k);}
 }}
 var rgb=c.rgb+detail;
 if(arg(9)>0.){rgb=clamp(c.rgb+detail*smoothstep(vec3(0.),vec3(.25),c.rgb),vec3(0.),vec3(1.));}
 out=vec4(rgb,c.a);
}
case 12u:{// PS 0.3% Gaussian: sigma≈amount/100 at16, twice at32; 8-bit scale fitted after rounding.
 let seed=bitcast<u32>(arg(2))^hash(id.x+id.y*size.x);var noise=vec3(0.);for(var channel=0u;channel<3u;channel++){let u=max(random(seed+channel*2u),1e-7);let v=random(seed+channel*2u+1u);noise[channel]=sqrt(-2.*log(u))*cos(6.2831853*v);}if(arg(1)>0.){noise=vec3(noise.x);}out=vec4(c.rgb+noise*arg(0),c.a);}
case 13u:{
 // PS Float Levels retains signed shadows and highlights outside the endpoints.
 var input=c.rgb;
 if(arg(5)==32768.){input=floor(input*32768.+.5)/32768.;}
 var t=(input-arg(0))/(arg(1)-arg(0));
 if(arg(5)>0.){t=clamp(t,vec3(0.),vec3(1.));}
 // Photoshop's 8-bit input-level LUT rounds before its gamma LUT.
 if(arg(5)==255.){t=floor(t*255.+.5)/255.;}
 var mapped=sign(t)*pow(abs(t),vec3(1./arg(2)));
 if(arg(5)>0.){
  // PS shadow toe: origin slope 2^gamma; power junction rounds to the 4096 grid.
  mapped=select(mapped,t*(arg(9)+t*(arg(8)+t*arg(7))),t<vec3(arg(6)));
 }
 out=vec4(arg(3)+(arg(4)-arg(3))*mapped,c.a);
}
// Measured PS: gamma-563/256 transfer with a linearly interpolated inverse LUT.
case 14u:{
 var rgb=c.rgb;
 for(var i=0u;i<3u;i++){
  var v=rgb[i];
  if(arg(0)==0.&&arg(1)==0.&&arg(2)==1.){continue;}
  if(arg(3)>0.){v=pow(max(v,0.),563./256.);}
  v=v*exp2(arg(0))+arg(1);
  if(arg(2)!=1.){
   if(arg(3)==0.&&arg(4)>0.&&v!=0.&&abs(v)<=3.4028234663852886e38){
    let parts=frexp(abs(v));let position=(parts.fract*2.-1.)*arg(4);
    let index=u32(floor(position));let fraction=position-f32(index);
    let left=stops[index];let right=stops[index+1u];
    let delta=right.x-left.x;
    let high=fma(fraction,delta,left.x);
    let low=fma(fraction,delta,left.x-high)+left.y+fraction*(right.y-left.y);
    let value=high+low;let residual=(high-value)+low;
    let scale=stops[u32(parts.exp-1+149)+u32(arg(4))+1u];
    let product=value*scale.x;
    let error=fma(value,scale.x,-product)+value*scale.y+residual*scale.x;
    v=sign(v)*ldexp(product+error,i32(scale.z));
   }else{v=sign(v)*pow(abs(v),1./arg(2));}
  }
  if(arg(3)>0.){
   let t=clamp(v,0.,1.)*4096.;let lo=floor(t);let f=t-lo;
   v=mix(pow(lo/4096.,256./563.),pow(min(lo+1.,4096.)/4096.,256./563.),f);
  }
  rgb[i]=v;
 }
 out=vec4(rgb,c.a);
}
case 15u:{
 var source=c.rgb;
 if(arg(3)==0.){
  if(arg(4)==32768.){
   // Recover the integer TIFF word first: multiplying normalized Float by
   // 32768 rounds high even words onto a false half-tie.
   source=floor((floor(source*65535.+.5)+1.)*.5)/32768.;
  }else if(arg(4)>0.){source=floor(source*arg(4)+.5)/arg(4);}
  if(arg(4)==255.){
   // The 8-bit lightness LUT truncates the signed distance, not its result.
   var units=floor(source*255.+.5);
   if(arg(2)>=0.){units+=floor((vec3(255.)-units)*arg(2));}
   else{units-=floor(units*(-arg(2)));}
   source=units/255.;
  }else{
   if(arg(2)>=0.){source=mix(source,vec3(1.),arg(2));}else{source*=1.+arg(2);}
   if(arg(4)>0.){source=floor(source*arg(4)+.5)/arg(4);}
  }
 }
 var v=hsl(source);
 if(arg(3)>0.){v.x=arg(0);v.y=clamp(arg(1),0.,1.);}
 else if(arg(4)==255.){
  // PS 8-bit hue uses 255-unit sextants, but reconstructs the middle
  // channel on a 256-unit grid. Round quotient and slider separately.
  let units=floor(source*255.+.5);
  let hi=max(units.r,max(units.g,units.b));let lo=min(units.r,min(units.g,units.b));let delta=hi-lo;
  var hue=0.;
  if(delta>0.){
   if(hi==units.r){hue=floor((units.g-units.b)*255./delta+.5);}
   else if(hi==units.g){hue=floor((units.b-units.r)*255./delta+.5)+510.;}
   else{hue=floor((units.r-units.g)*255./delta+.5)+1020.;}
  }
  hue+=arg(5);
  hue-=floor(hue/1530.)*1530.;
  let sector=floor(hue/255.);let fraction=hue-sector*255.;
  let odd=u32(sector)%2u==1u;
  let middle=select(fraction,255.-fraction,odd)/256.;
  v.x=(sector+select(middle,1.-middle,odd))/6.;
 }else{v.x=fract(v.x+arg(0));}
 var rgb=fromhsl(v);
 if(arg(3)>0.){
  if(arg(2)>=0.){rgb=mix(rgb,vec3(1.),arg(2));}else{rgb*=1.+arg(2);}
 }else{
  let lo=min(rgb.r,min(rgb.g,rgb.b));let hi=max(rgb.r,max(rgb.g,rgb.b));let gray=(hi+lo)*.5;
  var scale=1.+arg(1);
  if(arg(1)>0.){
   // Photoshop caps the reciprocal at 32767/256, including unclipped Float RGB.
   scale=1./max(1.-arg(1),256./32767.);
   if(arg(4)>0. && hi>lo){scale=min(scale,(1.-abs(2.*gray-1.))/(hi-lo));}
  }
  rgb=vec3(gray)+(rgb-vec3(gray))*scale;
  // PS's 8-bit full-desaturation LUT truncates the integer midpoint.
  if(arg(4)==255. && arg(1)==-1.){
   let units=floor(source*255.+.5);
   rgb=vec3(floor((max(units.r,max(units.g,units.b))+min(units.r,min(units.g,units.b)))*.5)/255.);
  }
 }
 out=vec4(rgb,c.a);
}
case 16u:{// Measured linear-HSV vibrance, followed by linear-RGB saturation.
 if(arg(0)!=0.||arg(1)!=0.){
  let input=select(c.rgb,floor((floor(c.rgb*65535.+.5)+1.)*.5)/32768.,arg(2)==32768.);
  var lin=vec3(icc_srgb_decode(input.r),icc_srgb_decode(input.g),icc_srgb_decode(input.b));
  if(arg(0)!=0.){
   let hi=max(lin.r,max(lin.g,lin.b));let lo=min(lin.r,min(lin.g,lin.b));
   if(hi>lo){let q=abs(arg(0));let t=lo/hi;let s=1.-t;let fade=max(1.-16.*hi,0.);let shadow=1.-fade*fade;
    let loss=lo*s*(2.-t+t*t)*(1.-hi)*shadow;var top=hi-q*loss;
    var saturation=s*(1.-q*.25)*((1.-q)+q*.5*shadow*s*(1.+s));
    if(arg(0)>0.){
     var skin=1.;if(lin.r==hi){skin=abs(lin.g-lin.b)/(hi-lo);if(lin.g>=lin.b){skin=clamp(4.*skin-2.,0.,1.);}}
     let protect=s*s+(1.-s*s)*skin;let amount=q+(1.-q)*protect;
     top=hi+loss*q*amount*.25;
     // CALIBRATE: red-protection floor .0285 fitted from the independent dense max/min oracle.
     let floor_protect=protect+.0285*(1.-protect);
     let attenuation=(5./6.)*(1.-s)*(1.-hi*(1.-s))*shadow*.5*(1.+floor_protect)*amount;
     saturation=s/(1.-q*attenuation);
    }
    let bottom=top*(1.-saturation);lin=vec3(bottom)+(lin-vec3(lo))*((top-bottom)/(hi-lo));
   }
  }
  if(arg(1)!=0.){let gray=dot(lin,vec3(.288,.712,0.));lin=clamp(mix(vec3(gray),lin,1.+arg(1)),vec3(0.),vec3(1.));}
  out=vec4(icc_srgb_encode(lin.r),icc_srgb_encode(lin.g),icc_srgb_encode(lin.b),c.a);
 }
}
case 17u:{
 var gray=(max(c.r,max(c.g,c.b))+min(c.r,min(c.g,c.b)))*.5;
 if(arg(0)==255.){
  let units=floor(c.rgb*255.+.5);
  gray=floor((max(units.r,max(units.g,units.b))+min(units.r,min(units.g,units.b)))*.5)/255.;
 }
 out=vec4(vec3(gray),c.a);
}
case 18u:{// PS default six-sector weights R40 Y60 G40 C60 B20 M80; corpus 16-bit max residual 7.12/32768 (fixed-point rounding).
 let v=hsl(c.rgb);let h=v.x*6.;let sector=u32(h)%6u;let weights=array<f32,7>(.4,.6,.4,.6,.2,.8,.4);let lo=min(c.r,min(c.g,c.b));let hi=max(c.r,max(c.g,c.b));out=vec4(vec3(lo+(hi-lo)*mix(weights[sector],weights[sector+1u],fract(h))),c.a);}
case 19u:{out=vec4(vec3(colorlum(c.rgb,arg(0))),c.a);}
case 20u:{
 let input=select(c.rgb,floor((floor(c.rgb*65535.+.5)+1.)*.5)/32768.,arg(1)==32768.);
 let value=floor(lum(input)*4096.+.5)/4096.;let n=u32(arg(0));var lab=stops[0].yzw;
 for(var i=1u;i<n;i++){
  let prev=stops[i-1u];let next=stops[i];
  if(value>=prev.x){
   let width=max((next.x-prev.x)*4096.,1.);let x=clamp((value-prev.x)*4096.,0.,width);
   let midpoint=ceil(width*.5);
   let t=select(select(.5+.5*(x-midpoint)/max(width-midpoint,1.),.5*x/midpoint,x<midpoint),1.,x>=width);
   let p0=stops[u32(max(i32(i)-2,0))].yzw;let p1=prev.yzw;let p2=next.yzw;let p3=stops[min(i+1u,n-1u)].yzw;
   // PS smoothness 4096: component-clamped Catmull-Rom in Oklab.
   lab=clamp(.5*(2.*p1+(-p0+p2)*t+(2.*p0-5.*p1+4.*p2-p3)*t*t+(-p0+3.*p1-3.*p2+p3)*t*t*t),min(p1,p2),max(p1,p2));
  }
 }
 let l=lab.x+.3963377774*lab.y+.2158037573*lab.z;
 let m=lab.x-.1055613458*lab.y-.0638541728*lab.z;
 let s=lab.x-.0894841775*lab.y-1.2914855480*lab.z;
 let l3=l*l*l;let m3=m*m*m;let s3=s*s*s;
 let rgb=vec3(4.0767416621*l3-3.3077115913*m3+.2309699292*s3,-1.2684380046*l3+2.6097574011*m3-.3413193965*s3,-.0041960863*l3-.7034186147*m3+1.7076147010*s3);
 out=vec4(clamp(vec3(encode(rgb.r),encode(rgb.g),encode(rgb.b)),vec3(0.),vec3(1.)),c.a);
}
case 21u:{let rgb=pow(max(vec3(0.),1.-exp(-3.*arg(0)*c.rgb)),vec3(1./arg(1)));out=vec4(floor(rgb*10000.+.5)/10000.,1.);}
case 22u:{let alpha=max(c.r,max(c.g,c.b));if(alpha>0.){out=vec4(c.rgb/alpha,alpha);}else{out=vec4(0.);}}
case 23u:{if(arg(0)>0.){out=floor(clamp(c,vec4(0.),vec4(1.))*arg(0)+.5)/arg(0);}}
case 24u:{
 // Local tone/color response plus image-relative, self-guided log-luminance recovery.
 // CALIBRATE: full Embers RGB38.70dB; residual bright-detail/color recovery, despite extraction55.54dB.
 let mode=u32(arg(1));
 let l=clamp(lum(c.rgb),0.,1.);let s=max(c.r,max(c.g,c.b))-min(c.r,min(c.g,c.b));
 if(mode==1u){out=vec4(l,s,0.,c.a);}
 else if(mode==7u){
  let position=(fp+.5)*source/vec2<f32>(size)-.5;let origin=vec2<i32>(floor(position));let fraction=fract(position);var sum=vec3(0.);
  for(var y=0;y<2;y++){for(var x=0;x<2;x++){
   let tap=load(a,origin+vec2(x,y)).rgb;let rgb=vec3(decode(tap.r),decode(tap.g),decode(tap.b));
   let weight=select(1.-fraction.x,fraction.x,x==1)*select(1.-fraction.y,fraction.y,y==1);sum+=rgb*weight;
  }}out=vec4(sum,1.);
 }
 else if(mode==2u){
  var rgb=c.rgb;if(arg(0)>0.){rgb=vec3(decode(rgb.r),decode(rgb.g),decode(rgb.b));}
  let value=log2(max(dot(rgb,vec3(.2126,.7152,.0722)),.0006));out=vec4(value,value*value,0.,1.);
 }else if(mode==3u){
  let radius=i32(arg(2));let direction=vec2<i32>(i32(arg(3)),i32(arg(4)));var sum=vec4(0.);
  for(var k=-radius;k<=radius;k++){sum+=load(a,p+k*direction);}out=sum/f32(2*radius+1);
 }else if(mode==4u){
  let mean=load(b,p);let variance=max(mean.g-mean.r*mean.r,0.);let coefficient=variance/(variance+8.);
  out=vec4(coefficient,(1.-coefficient)*mean.r,0.,1.);
 }else if(mode==5u){
  let adaptive=linear(b,(fp+.5)*vec2<f32>(textureDimensions(b))/source-.5);
  out=vec4(c.rg,adaptive.rg);
 }else{
  let guide=load(b,p);let base=guide.r;let chroma=guide.g;
  var neutral=array<f32,6>(-.0508271128,2.54195235,-12.3367706,24.7736787,-19.7112036,4.86399011);
  var correction=array<f32,10>(.186460012,.0648676969,.166432142,-.293584691,-.345312016,.0540632944,.163031159,-.297765206,.7111935,-.33284872);
  if(arg(0)==0.){
   neutral=array<f32,6>(3.92805078,-28.9683362,103.814284,-175.499224,139.478309,-42.4170706);
   correction=array<f32,10>(.941590379,-.432951597,.357234274,-.32085173,-1.54717155,.531581375,-.0690353821,-.141425236,2.68406245,-1.94010943);
  }
  var loss=neutral[5];for(var i=4;i>=0;i--){loss=loss*l+neutral[u32(i)];}loss*=l;
  let features=array<f32,10>(l,l*l,s,s*s,base,chroma,l*s,l*base,base*chroma,chroma*chroma);
  var adjustment=0.;for(var i=0u;i<10u;i++){adjustment+=features[i]*correction[i];}
  loss=clamp(mix(loss,adjustment,min(2.*chroma/max(base,.01),1.)),0.,.7);
  var rgb=max(c.rgb-vec3(loss),vec3(0.));var original=c.rgb;
  if(arg(0)==0.){rgb=c.rgb*(1.-loss);}else{
   rgb=vec3(decode(rgb.r),decode(rgb.g),decode(rgb.b));
   original=vec3(decode(original.r),decode(original.g),decode(original.b));
  }
  let log_y=log2(max(dot(original,vec3(.2126,.7152,.0722)),.0006));
  let relative=guide.b*log_y+guide.a-load(mask,vec2(0)).r;
  let position=clamp(relative-4.,0.,5.);let index=min(u32(floor(position)),4u);
  let gains=array<f32,6>(0.,-1.99206661,-2.38074202,-2.90063828,-4.34677746,-7.83526378);
  rgb*=exp2(mix(gains[index],gains[index+1u],position-f32(index)));
  if(arg(0)>0.){rgb=vec3(encode(rgb.r),encode(rgb.g),encode(rgb.b));}out=vec4(rgb,c.a);
 }
}
// PS combines ordinary Fill and Opacity into one rounded 8-bit coverage, at every depth.
case 25u:{
 let top=region_load(b,p);let mode=u32(arg(0));let opacity=clamp(arg(1),0.,1.);let fill=clamp(arg(2),0.,1.);
 var src=top.rgb;var coverage=top.a*opacity;
 if(mode==2u||mode==3u){src*=fill;if(arg(3)==255.){src=floor(src*255.+.5)/255.;}}
 else{coverage=top.a*floor(opacity*fill*255.+.5)/255.;}
 var f=blendfn(c.rgb,src,mode,arg(3));
 // PS byte layers round the blend result before opacity interpolation.
 if(arg(3)==255.){f=floor(f*255.+.5)/255.;}
 let alpha=coverage+c.a*(1.-coverage);var rgb=vec3(0.);
 if(alpha>0.){rgb=(coverage*(1.-c.a)*src+coverage*c.a*f+(1.-coverage)*c.a*c.rgb)/alpha;}
 out=vec4(rgb,alpha);if(arg(3)>0.){out=clamp(out,vec4(0.),vec4(1.));}
}
case 26u:{var src=load(b,p).rgb;if(arg(2)>0.){src=1.-src;}let f=blendfn(c.rgb,src,u32(arg(0)),arg(3));out=vec4(mix(c.rgb,f,arg(1)),c.a);}
case 27u:{out=vec4(vec3(c[u32(arg(0))]),1.);}
// Per-component select, not out[channel]=...: DX12's FXC rejects writes at a runtime index (X3500).
case 28u:{let channel=u32(arg(0));let f=blendfn(vec3(c[channel]),vec3(load(b,p).r),u32(arg(1)),0.);out=select(out,vec4(mix(c[channel],f.x,arg(2))),vec4<u32>(0u,1u,2u,3u)==vec4(channel));}
case 29u:{out.a=c.a*load(b,p).r;}
case 30u:{out=mix(c,load(b,p),load(mask,p).r);}
// Measured SDR→Float passes through Photoshop's 15-bit-plus-one integer grid.
// PS downconversion to Eight adds spatial dither; this models its undithered mean.
case 31u:{var rgb=c.rgb;for(var i=0u;i<3u;i++){if(arg(0)>0.&&arg(1)==0.){rgb[i]=decode(floor(clamp(rgb[i],0.,1.)*32768.+.5)/32768.);}if(arg(0)==0.&&arg(1)>0.){rgb[i]=encode(rgb[i]);}}out=vec4(rgb,c.a);if(arg(1)>0.){out=floor(clamp(out,vec4(0.),vec4(1.))*arg(1)+.5)/arg(1);}}
case 35u:{
 let q=vec2(arg(0),arg(1))+(vec2<f32>(id.xy)+.5)*arg(2)-.5;
 if(arg(3)>0.){out=linear(a,q);}else{out=load(a,vec2<i32>(floor(q+.5)));}
}
case 32u:{
 // Forward shear, with enough transverse guard pixels for inverse cubic taps.
 let vertical=arg(1)>0.;let major=select(p.x,p.y,vertical);let minor=select(p.y,p.x,vertical);
 let phase=floor((f32(major)+.5)*arg(0)*128.)/128.;
 let g=fract(ceil((f32(major)+.5)*arg(0)*128.)/128.);
 let base=minor+i32(arg(2))-i32(floor(phase));var sum=vec4(0.);
 if(arg(3)>0.){
  for(var k=-1;k<=2;k++){sum+=motion_load(select(vec2(major,base-k),vec2(base-k,major),vertical))*motion_cubic(f32(k)-g);}
  out=floor(clamp(sum,vec4(0.),vec4(1.))*arg(3)+.5)/arg(3);
 }else{
  let first=motion_load(select(vec2(major,base),vec2(base,major),vertical));
  let second=motion_load(select(vec2(major,base-1),vec2(base-1,major),vertical));
  out=mix(first,second,g);
 }
}
case 33u:{
 // Fuse offsets k, 2k, 4k and 8k without changing their f32 addition tree.
 let vertical=arg(0)>0.;let major=select(p.x,p.y,vertical);let offset=i32(arg(1));
 let step=select(vec2(offset,0),vec2(0,offset),vertical);
 if(major>=offset){out=c+load(a,p-step);}
 if(arg(2)>0.&&major>=2*offset){
  var previous=load(a,p-2*step);
  if(major>=3*offset){previous=previous+load(a,p-3*step);}
  out=out+previous;
 }
 if(arg(3)>0.&&major>=4*offset){
  var previous=load(a,p-4*step);
  if(major>=5*offset){previous=previous+load(a,p-5*step);}
  if(major>=6*offset){
   var tail=load(a,p-6*step);
   if(major>=7*offset){tail=tail+load(a,p-7*step);}
   previous=previous+tail;
  }
  out=out+previous;
 }
 if(arg(4)>0.&&major>=8*offset){
  var previous=load(a,p-8*step);
  if(major>=9*offset){previous=previous+load(a,p-9*step);}
  if(major>=10*offset){
   var tail=load(a,p-10*step);
   if(major>=11*offset){tail=tail+load(a,p-11*step);}
   previous=previous+tail;
  }
  if(major>=12*offset){
   var tail=load(a,p-12*step);
   if(major>=13*offset){tail=tail+load(a,p-13*step);}
   if(major>=14*offset){
    var end=load(a,p-14*step);
    if(major>=15*offset){end=end+load(a,p-15*step);}
    tail=tail+end;
   }
   previous=previous+tail;
  }
  out=out+previous;
 }
}
case 34u:{
 let vertical=arg(1)>0.;let major=select(p.x,p.y,vertical);let minor=select(p.y,p.x,vertical);
 let phase=floor((f32(major)+.5)*arg(0)*128.)/128.;let f=fract(phase);
 let z=minor+i32(floor(phase))-i32(arg(2));var sum=vec4(0.);
 // Only this strip's non-overlapping interior owns the output pixel.
 if(arg(5)>0.&&(z<2||z>=i32(arg(5))+2)){return;}
 if(arg(3)>0.){
  for(var j=-1;j<=2;j++){sum+=motion_box(major,z+j,vertical)*motion_cubic(f32(j)-f);}
  out=clamp(sum,vec4(0.),vec4(1.));
 }else{
  out=mix(motion_box(major,z,vertical),motion_box(major,z+1,vertical),f);
 }
}
default:{}
}
if(u.head.w>0.){out=floor(clamp(out,vec4(0.),vec4(1.))*u.head.w+.5)/u.head.w;}
textureStore(dst,p,out);
}

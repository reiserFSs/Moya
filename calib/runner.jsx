#target photoshop
(function () {
    var root = MOYA_ROOT;
    function read(f) { f.encoding='UTF8'; f.open('r'); var s=f.read(); f.close(); return s; }
    function write(f,s) { f.encoding='UTF8'; f.open('w'); f.write(s); f.close(); }
    var manifestFile = new File(root+'/manifest.json');
    var manifest = eval('('+read(manifestFile)+')');
    // ExtendScript lacks JSON on some Photoshop installations.
    function json(x) {
        if (x===null) return 'null';
        if (typeof x==='string') return '"'+x.replace(/\\/g,'\\\\').replace(/"/g,'\\"').replace(/\r/g,'\\r').replace(/\n/g,'\\n')+'"';
        if (typeof x==='number'||typeof x==='boolean') return String(x);
        var a=[],k;
        if (x instanceof Array) { for(k=0;k<x.length;k++)a.push(json(x[k])); return '['+a.join(',')+']'; }
        for(k in x) if(x.hasOwnProperty(k))a.push(json(k)+':'+json(x[k]));
        return '{'+a.join(',')+'}';
    }
    function id(s) { if(s.charAt(0)==='$')return charIDToTypeID(s.substr(1)); return stringIDToTypeID(s); }
    function ref(v) { var r=new ActionReference(); if (!(v instanceof Array))v=[v]; for(var i=0;i<v.length;i++){var t=v[i]; if(t._value!==undefined && t._enum)r.putEnumerated(id(t._ref),id(t._enum),id(t._value));else if(t._id!==undefined)r.putIdentifier(id(t._ref),t._id);else if(t._index!==undefined)r.putIndex(id(t._ref),t._index);else if(t._name!==undefined)r.putName(id(t._ref),t._name);else r.putClass(id(t._ref));}return r; }
    function put(d,k,v,list) {
        var key=list?null:id(k), method=function(name,args){ if(!list)args.unshift(key); d[name].apply(d,args); };
        if(v===null||v===undefined)return;
        if(v instanceof Array){if(v.length && v[0]._ref){method('putReference',[ref(v)]);return;}var l=new ActionList();for(var i=0;i<v.length;i++)put(l,null,v[i],true);method('putList',[l]);}
        else if(typeof v==='boolean')method('putBoolean',[v]);
        else if(typeof v==='string')method('putString',[v]);
        else if(typeof v==='number')method(v===Math.floor(v)?'putInteger':'putDouble',[v]);
        else if(v._unit)method('putUnitDouble',[id(v._unit),v._value]);
        else if(v._ref)method('putReference',[ref(v._ref instanceof Array ? v._ref : v)]);
        else if(v._enum)method('putEnumerated',[id(v._enum),id(v._value)]);
        else {var o=descriptor(v);method('putObject',[id(v._obj||k),o]);}
    }
    function descriptor(o){var d=new ActionDescriptor();for(var k in o)if(o.hasOwnProperty(k)&&k.charAt(0)!=='_'&&k!=='centerSpace')put(d,k,o[k],false);return d;}
    function action(op,args){return executeAction(id(op),descriptor(args||{}),DialogModes.NO);}
    var oldDialogs=app.displayDialogs;
    var initialDocuments=app.documents.length;
    app.displayDialogs=DialogModes.NO;
    var started=new Date().getTime(), succeeded=0, rejected=0;
    if(!manifest.captureRuns){manifest.captureRuns=[];if(manifest.capture)manifest.captureRuns.push(manifest.capture);}
    manifest.photoshopVersion=app.version;
    manifest.colorSettings={name:app.colorSettings};
    // Keep full, typed color-settings descriptor to expose working RGB, gamma and dither.
    try {
        var rr=new ActionReference();rr.putProperty(id('property'),id('colorSettings'));rr.putEnumerated(id('application'),id('ordinal'),id('targetEnum'));
        var cs=executeActionGet(rr).getObjectValue(id('colorSettings'));
        function descJSON(d){var o={};for(var i=0;i<d.count;i++){var k=d.getKey(i),n=typeIDToStringID(k)||typeIDToCharID(k),t=d.getType(k);try{if(t===DescValueType.BOOLEANTYPE)o[n]=d.getBoolean(k);else if(t===DescValueType.STRINGTYPE)o[n]=d.getString(k);else if(t===DescValueType.DOUBLETYPE)o[n]=d.getDouble(k);else if(t===DescValueType.INTEGERTYPE)o[n]=d.getInteger(k);else if(t===DescValueType.ENUMERATEDTYPE)o[n]=typeIDToStringID(d.getEnumerationValue(k));else if(t===DescValueType.OBJECTTYPE)o[n]=descJSON(d.getObjectValue(k));else o[n]='type '+t;}catch(e){o[n]='ERROR '+e;}}return o;}
        manifest.colorSettings.values=descJSON(cs);
    }catch(e){manifest.colorSettings.error=String(e);}
    function depth(doc,n){if(n===8)doc.bitsPerChannel=BitsPerChannelType.EIGHT;else if(n===16)doc.bitsPerChannel=BitsPerChannelType.SIXTEEN;else doc.bitsPerChannel=BitsPerChannelType.THIRTYTWO;}
    function topLayer(c,doc){var t=app.open(new File(root+'/'+c.top.input));if(t===doc)t=doc.duplicate('Moya temporary source',true);t.activeLayer.duplicate(doc,ElementPlacement.PLACEATBEGINNING);t.close(SaveOptions.DONOTSAVECHANGES);app.activeDocument=doc;doc.activeLayer=doc.layers[0];action('set',{'null':[{_ref:'layer',_enum:'ordinal',_value:'targetEnum'}],to:{_obj:'layer',mode:{_enum:'blendMode',_value:c.top.blend},opacity:{_unit:'percentUnit',_value:c.top.opacity===undefined?100:c.top.opacity},fillOpacity:{_unit:'percentUnit',_value:c.top.fill===undefined?100:c.top.fill}}});}
    try {
        for(var i=0;i<manifest.cases.length;i++){
            var c=manifest.cases[i],doc=null;
            if(MOYA_OP&&c.op!==MOYA_OP)continue;if(MOYA_CASE&&c.id!==MOYA_CASE)continue;
            var tick=new Date().getTime();
            try{
                doc=app.open(new File(root+'/'+c.input));
                if(c.top)topLayer(c,doc);
                if(c.op==='layerComposite'){doc.flatten();}
                else if(c.op==='copyToLayer'){action('copyToLayer',c.args);doc.flatten();}
                else if(c.op==='convertMode'){action(c.op,c.args);}
                else if(c.op==='applyImageEvent' && !c.args['with']){
                    var source=doc.activeLayer;source.name='Moya Calibration Source';var target=doc.layers[doc.layers.length-1];doc.activeLayer=target;
                    var calc={_obj:'calculation',to:[{_ref:'channel',_enum:'channel',_value:'RGB'},{_ref:'layer',_name:source.name}],calculation:{_enum:'calculationType',_value:c.args.calculation},opacity:{_unit:'percentUnit',_value:c.args.opacity===undefined?100:c.args.opacity},invert:!!c.args.invert};
                    action('applyImageEvent',{'with':calc});source.remove();
                }
                else {action(c.op,c.args);}
                if(doc.bitsPerChannel!==(c.depth===8?BitsPerChannelType.EIGHT:c.depth===16?BitsPerChannelType.SIXTEEN:BitsPerChannelType.THIRTYTWO))depth(doc,c.depth);
                if(doc.layers.length>1)doc.flatten();
                var file=new File(root+'/'+c.output);file.parent.create();var options=new TiffSaveOptions();options.imageCompression=TIFFEncoding.TIFFZIP;options.byteOrder=ByteOrder.IBM;options.embedColorProfile=true;options.layers=false;options.alphaChannels=false;
                doc.saveAs(file,options,true,Extension.LOWERCASE);
                c.capture={status:'ok',milliseconds:new Date().getTime()-tick};succeeded++;
            }catch(e){c.capture={status:'rejected',error:String(e),number:e.number,line:e.line,milliseconds:new Date().getTime()-tick};rejected++;}
            finally{if(doc){try{doc.close(SaveOptions.DONOTSAVECHANGES);}catch(e){}}while(app.documents.length>initialDocuments){app.activeDocument.close(SaveOptions.DONOTSAVECHANGES);}}
            if(i%25===0)write(manifestFile,json(manifest));
        }
    }finally{app.displayDialogs=oldDialogs;manifest.capture={milliseconds:new Date().getTime()-started,succeeded:succeeded,rejected:rejected,op:MOYA_OP,caseId:MOYA_CASE};manifest.captureRuns.push(manifest.capture);write(manifestFile,json(manifest));}
    return json(manifest.capture);
})();

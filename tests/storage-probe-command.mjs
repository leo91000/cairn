export function storageProbeCommand({
  workspace,
  first,
  benchmark = false,
  soak = false,
  inbox = '/run/cairn-chat/messages.json',
}) {
  return `
      const fs=require('node:fs'),assert=require('node:assert/strict'),cp=require('node:child_process');
      const file=${JSON.stringify(`${workspace}/saved`)};
      const unused=${JSON.stringify(`${workspace}/unused`)};
      const hash=bytes=>require('node:crypto').createHash('sha256').update(bytes).digest('hex');
      if(${first}) {fs.writeFileSync(file,'journal survives'); fs.writeFileSync(${JSON.stringify(`${workspace}/unused`)},require('node:crypto').randomBytes(32*1024*1024)); fs.writeFileSync(unused+'.sha256',hash(fs.readFileSync(unused))); cp.execFileSync('sync');}
      const started=performance.now();
      assert.equal(fs.readFileSync(file,'utf8'),'journal survives');
      const savedReadMs=performance.now()-started;
      console.log('storage.ready'); setInterval(()=>console.log('storage.tick'),1000);
      let pressure='idle';
      setInterval(()=>{
        let messages;
        try {messages=JSON.parse(fs.readFileSync(${JSON.stringify(inbox)},'utf8'));}
        catch(error) {if(error.code==='ENOENT'||error instanceof SyntaxError)return;throw error;}
        if(!Array.isArray(messages))return;
        if(pressure==='idle'&&messages.some(message=>message?.text==='exhaust')){
          const before=fs.statfsSync(${JSON.stringify(workspace)});
          assert.ok(before.blocks*before.bsize<=512*1024*1024,'pressure workload requires its own bounded guest data filesystem');
          const fd=fs.openSync(file+'.pressure','w'),data=Buffer.alloc(1024*1024,42),at=performance.now(),cpu=process.cpuUsage();
          let written=0,code;
          try{while(written<600*1024*1024){const count=fs.writeSync(fd,data);assert.ok(count>0);written+=count;fs.fsyncSync(fd);}}
          catch(error){code=error.code;}
          finally{fs.closeSync(fd);}
          assert.equal(code,'ENOSPC','guest data capacity must reject further allocation');
          assert.equal(fs.readFileSync(file,'utf8'),'journal survives');
          const usage=process.cpuUsage(cpu);pressure='full';
          console.log('storage.enospc '+JSON.stringify({code,writtenBytes:written,wallMs:performance.now()-at,cpuMs:(usage.user+usage.system)/1000}));
          return;
        }
        if(pressure==='full'&&messages.some(message=>message?.text==='reclaim')){
          fs.unlinkSync(file+'.pressure');cp.execFileSync('sync');
          assert.equal(fs.readFileSync(file,'utf8'),'journal survives');pressure='reclaimed';
          console.log('storage.reclaimed');return;
        }
        if(!messages.some(message=>message?.text==='finish'))return;
        cp.execFileSync('sync'); console.log('storage.finished'); process.exit(0);
      },50);
      if(${soak}) {
        const fd=fs.openSync(file+'.load','w+'), data=Buffer.alloc(4096), read=Buffer.alloc(4096);
        let sequence=0, maximumMs=0;const latencies=[];
        setInterval(()=>{
          sequence++;data.fill(sequence%251);
          const offset=(sequence%128)*4096,at=performance.now();
          assert.equal(fs.writeSync(fd,data,0,data.length,offset),data.length);fs.fsyncSync(fd);
          assert.equal(fs.readSync(fd,read,0,read.length,offset),read.length);assert.deepEqual(read,data);
          const ms=performance.now()-at;maximumMs=Math.max(maximumMs,ms);latencies.push(ms);
          if(latencies.length>5000)latencies.shift();
        },10);
        setInterval(()=>{
          const sorted=[...latencies].sort((a,b)=>a-b);
          console.log('storage.soak '+JSON.stringify({writes:sequence,maximumMs,p99Ms:sorted[Math.floor(sorted.length*.99)]||0}));
        },1000);
      }
      if(${benchmark}) {
        const control=setInterval(()=>{
          // Guest tar imports can briefly remove the file or expose partial JSON.
          // Match the real adapters: poll again until a complete message arrives.
          let messages;
          try {messages=JSON.parse(fs.readFileSync(${JSON.stringify(inbox)},'utf8'));}
          catch(error) {if(error.code==='ENOENT'||error instanceof SyntaxError)return;throw error;}
          if(!Array.isArray(messages)||!messages.some(message=>message?.text==='measure'))return;

          clearInterval(control);
          const fd=fs.openSync(unused,'r'), buffer=Buffer.alloc(4096), firstReadsMs=[];
          for(const offset of [0,8*1024*1024,16*1024*1024]) {
            const at=performance.now(); assert.equal(fs.readSync(fd,buffer,0,buffer.length,offset),4096);
            firstReadsMs.push(performance.now()-at);
          }
          fs.closeSync(fd);
          const at=performance.now(), bytes=fs.readFileSync(unused), elapsedMs=performance.now()-at;
          assert.equal(bytes.length,32*1024*1024);
          assert.equal(hash(bytes),fs.readFileSync(unused+'.sha256','utf8'));
          console.log('storage.metrics '+JSON.stringify({savedReadMs,firstReadsMs,sequentialMs:elapsedMs,mibPerSecond:32/(elapsedMs/1000)}));
        },50);
      }
    `
}

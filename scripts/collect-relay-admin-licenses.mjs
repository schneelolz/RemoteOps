import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const repository=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const frontend=path.join(repository,'relay-admin');
const output=path.resolve(process.argv[2]);
const lock=JSON.parse(fs.readFileSync(path.join(frontend,'package-lock.json'),'utf8'));
const dependencies=[];
const notices=['RemoteOps Relay Admin 前端第三方许可证\n'];
for(const [relative,entry] of Object.entries(lock.packages)){
  if(!relative||entry.dev)continue;
  const directory=path.join(frontend,relative),pkg=JSON.parse(fs.readFileSync(path.join(directory,'package.json'),'utf8'));
  const licenses=fs.readdirSync(directory).filter(name=>/^(license|licence|copying|notice)(\.|-|$)/i.test(name)&&fs.statSync(path.join(directory,name)).isFile());
  if(!licenses.length && pkg.name!=='@vue/devtools-api')throw new Error('Missing license for '+pkg.name);
  dependencies.push({name:pkg.name,version:pkg.version,license:entry.license||pkg.license,integrity:entry.integrity});
  for(const filename of licenses)notices.push(`\n===== ${pkg.name} ${pkg.version}: ${filename} =====\n`+fs.readFileSync(path.join(directory,filename),'utf8'));
  if(!licenses.length)notices.push(`\n===== ${pkg.name} ${pkg.version}: upstream LICENSE =====\n`+fs.readFileSync(path.join(frontend,'legal/vue-devtools-6.6.4-LICENSE'),'utf8'));
}
fs.mkdirSync(output,{recursive:true});
fs.copyFileSync(path.join(repository,'LICENSE'),path.join(output,'LICENSE'));
fs.writeFileSync(path.join(output,'THIRD-PARTY-NOTICES.txt'),notices.join('\n'));
fs.writeFileSync(path.join(output,'dependencies.json'),JSON.stringify(dependencies,null,2));
console.log('Frontend license records:',dependencies.length);

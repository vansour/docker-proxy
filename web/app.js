(function(){
  const domain = location.hostname || 'docker.gitvansour.top';
  // If you host the web UI on same domain as proxy, use that. Otherwise change domain variable above.

  function $(id){return document.getElementById(id)}

  function parseImage(input){
    // trim
    input = input.trim();
    if(!input) return null;
    // split digest
    let digest = null;
    if(input.includes('@')){
      const parts = input.split('@');
      input = parts[0];
      digest = parts[1];
    }
    // find tag: the last ':' after last '/'
    const lastSlash = input.lastIndexOf('/');
    const lastColon = input.lastIndexOf(':');
    let name = input;
    let tag = 'latest';
    if(lastColon > lastSlash){
      tag = input.slice(lastColon + 1);
      name = input.slice(0, lastColon);
    }
    return {raw:input, name, tag, digest};
  }

  function genPullCmd(img){
    // The proxy expects the client to pull from domain/<image>
    return `docker pull ${domain}/${img.name}:${img.tag}`;
  }

  function genV2Probe(){
    return `${location.protocol}//${domain}/v2/`;
  }

  function genManifestUrl(img){
    // URL encode components
    const repo = encodeURIComponent(img.name);
    const ref = encodeURIComponent(img.tag);
    return `${location.protocol}//${domain}/v2/${repo}/manifests/${ref}`;
  }

  function genCurlExamples(img){
    const manifest = genManifestUrl(img);
    const curl1 = `curl -v -H "Accept: application/vnd.docker.distribution.manifest.v2+json" ${manifest}`;
    const curl2 = `curl -v ${location.protocol}//${domain}/v2/`;
    return `${curl1}\n\n${curl2}`;
  }

  function copyText(id){
    const el = $(id);
    if(!el) return;
    const text = el.innerText || el.textContent || el.value || '';
    navigator.clipboard && navigator.clipboard.writeText(text).then(()=>{
      // brief highlight
      el.style.transition = 'background .2s';
      el.style.background = '#eef';
      setTimeout(()=>el.style.background = '',400);
    }).catch(()=>alert('复制失败'));
  }

  document.addEventListener('DOMContentLoaded', ()=>{
    const genBtn = $('gen-btn');
    const input = $('image-input');
    const results = $('results');
    genBtn.addEventListener('click', ()=>{
      const img = parseImage(input.value);
      if(!img){ alert('请输入镜像名'); return; }
      $('pull-cmd').innerText = genPullCmd(img);
      $('v2-url').innerText = genV2Probe();
      $('manifest-url').innerText = genManifestUrl(img);
      $('curl-examples').innerText = genCurlExamples(img);
      results.classList.remove('hidden');
    });

    document.querySelectorAll('.copy-btn').forEach(b=>{
      b.addEventListener('click', (e)=>{
        const t = e.currentTarget.getAttribute('data-target');
        copyText(t);
      });
    });

    // allow Enter key on input
    input.addEventListener('keydown', (e)=>{
      if(e.key === 'Enter') { genBtn.click(); }
    });
  });
})();

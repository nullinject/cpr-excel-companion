"""Exercise the compiled listener's fail-closed boundary without contacting upstream."""
import base64,hashlib,hmac,json,os,pathlib,socket,subprocess,tempfile,time,urllib.request,urllib.error
root=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory() as folder:
 folder=pathlib.Path(folder);secret=os.urandom(48);(folder/'secret').write_bytes(secret)
 (folder/'accounts').write_text(json.dumps({'accounts':{'test-account':{'proxy':None,'direct':False}}}))
 with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
 env={**os.environ,'EXCEL_BRIDGE_LISTEN':f'127.0.0.1:{port}','EXCEL_BRIDGE_SECRET_FILE':str(folder/'secret'),'EXCEL_BRIDGE_ACCOUNT_MAP':str(folder/'accounts')}
 process=subprocess.Popen([str(root/'target/debug/cpr-excel-companion')],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
 def request(path,body=None,headers=None):
  req=urllib.request.Request(f'http://127.0.0.1:{port}'+path,data=body,headers=headers or {})
  try:
   with urllib.request.urlopen(req,timeout=2) as r:return r.status,r.read()
  except urllib.error.HTTPError as e:return e.code,e.read()
 def signed(account):
  encoded=base64.urlsafe_b64encode(json.dumps({'account':account,'scope':'test-only','request_id':'test-request','excel':True,'expires':int(time.time())+60}).encode()).rstrip(b'=')
  signature=base64.urlsafe_b64encode(hmac.new(secret,encoded,hashlib.sha256).digest()).rstrip(b'=')
  return (encoded+b'.'+signature).decode()
 try:
  for _ in range(100):
   try:
    if request('/healthz')[0]==204:break
   except OSError:pass
   if process.poll() is not None:raise AssertionError('bridge exited before health check')
   time.sleep(.02)
  else:raise AssertionError('bridge did not become ready')
  path='/backend-api/codex/responses'
  assert request(path,b'{}')[0]==503
  assert request(path,b'{}',{'x-excel-bridge-context':'forged'})[0]==503
  assert request(path,b'{}',{'x-excel-bridge-context':signed('unknown')})[0]==503
  assert request(path,b'{}',{'x-excel-bridge-context':signed('test-account')})[0]==503
  assert request(path,b'not json',{'x-excel-bridge-context':signed('test-account')})[0]==400
  assert request('/arbitrary-target')[0]==404
  print('PASS: health and 6 fail-closed HTTP checks; no upstream traffic')
 finally:
  process.terminate()
  try:process.wait(timeout=3)
  except subprocess.TimeoutExpired:process.kill();process.wait()

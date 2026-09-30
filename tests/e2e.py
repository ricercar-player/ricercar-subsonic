#!/usr/bin/env python3
"""End-to-end test of the plugin against a live Navidrome (see navidrome.sh):
JSON-RPC over stdio, the sign-in page over HTTP, the streams with ffprobe."""
import json, re, subprocess, sys, threading, queue, urllib.request, time, os, stat
S=sys.argv[1]; BIN=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "ricercar-subsonic")
DATA=S+"/pdata"; os.makedirs(DATA, exist_ok=True)
OUT={"device":"hw:9,0","bit_perfect":True,"max_rate":96000,"max_bits":24,"rates":[44100,48000,88200,96000]}
class P:
    def __init__(s):
        s.p=subprocess.Popen([BIN,"--server","127.0.0.1:4533"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(S+"/plugin.log","a"),text=True)
        s.q={}; s.notes=queue.Queue(); s.n=0
        threading.Thread(target=s.read,daemon=True).start()
    def read(s):
        for l in s.p.stdout:
            m=json.loads(l)
            if "id" in m: s.q[m["id"]].put(m)
            else: s.notes.put(m)
    def call(s,method,params=None):
        s.n+=1; i=s.n; s.q[i]=queue.Queue()
        s.p.stdin.write(json.dumps({"jsonrpc":"2.0","id":i,"method":method,"params":params or {}})+"\n"); s.p.stdin.flush()
        m=s.q[i].get(timeout=20); return m.get("result", m.get("error"))
    def notify(s,method,params):
        s.p.stdin.write(json.dumps({"jsonrpc":"2.0","method":method,"params":params})+"\n"); s.p.stdin.flush()
def post(url,body):
    r=urllib.request.Request(url,data=json.dumps(body).encode(),headers={"Content-Type":"application/json"})
    return json.load(urllib.request.urlopen(r))
ok=0
def check(c,msg):
    global ok
    print(("PASS " if c else "FAIL ")+msg); ok+= 0 if c else 1
for f in ("auth.json",):
    try: os.remove(DATA+"/"+f)
    except FileNotFoundError: pass
p=P()
init=p.call("initialize",{"protocol":1,"data_dir":DATA,"locale":"fr-FR","output":OUT})
check(init["capabilities"]["library"] and init["plugin"]["id"]=="subsonic","initialize")
caps=init["capabilities"]; check(all(caps.get(k) for k in ("lyrics","playlist_edit","details","radio")),"new capabilities")
sets={x["key"]:x for x in init.get("settings",[])}; check(set(sets)=={"report_playback","transcode"} and sets["report_playback"]["label"]=="Signaler mes écoutes" and sets["transcode"]["default"]=="auto","settings declared (fr)")
check(p.call("auth.status")["state"]=="signed_out","signed out at first")
check(p.call("browse.root").get("code")==-32001,"browse before sign-in -> auth_required")
b=p.call("auth.begin"); url=b["url"]
html=urllib.request.urlopen(url).read().decode(); check("Adresse du serveur" in html and "127.0.0.1:4533" in html,"login page (fr, hint)")
try: urllib.request.urlopen(url.rsplit("/",2)[0]+"/wrong/"); check(False,"bad secret refused")
except urllib.error.HTTPError as e: check(e.code==404,"bad secret refused")
c=post(url+"check",{"server":"127.0.0.1:4533"}); check(c["ok"] and c["name"].startswith("Navidrome"),"probe: "+str(c.get("name")))
check(not post(url+"check",{"server":"127.0.0.1:9"})["ok"],"probe unreachable fails")
bad=post(url+"password",{"server":c["server"],"user":"admin","password":"nope"}); check(not bad["ok"],"wrong password: "+bad.get("error",""))
good=post(url+"password",{"server":c["server"],"user":"admin","password":"sesame"}); check(good["ok"] and good["done"],"password sign-in")
n=p.notes.get(timeout=5); check(n["method"]=="auth.changed" and n["params"]["state"]=="signed_in","auth.changed: "+json.dumps(n["params"]["account"],ensure_ascii=False))
a=json.load(open(DATA+"/auth.json")); mode=stat.S_IMODE(os.stat(DATA+"/auth.json").st_mode)
check(a["auth"]["method"]=="token" and "sesame" not in json.dumps(a) and mode==0o600,"auth.json: token, no password, mode %o"%mode)
A="u=admin&p=sesame&v=1.16.1&c=t&f=json"
root=p.call("browse.root"); check([x["ref"] for x in root["sections"]]==["recent","albums","artists","playlists","favorites","frequent"],"root sections")
check([x["ref"] for x in root["home"]]==["recent","played","frequent","random"] and all(x["browsable"] and x["title"] for x in root["home"]),"home shelves "+str([x["title"] for x in root["home"]]))
for h in root["home"]:
    r=p.call("browse.list",{"ref":h["ref"],"offset":0,"limit":50}); ks={x["kind"] for x in r.get("items",[])}
    check("items" in r and ks<={"album","playlist"} and (h["ref"]!="random" or not r["has_more"]),"home %s: %d albums"%(h["ref"],len(r.get("items",[]))))
check(len(p.call("browse.list",{"ref":"recent","offset":0,"limit":50})["items"])==2 and len(p.call("browse.list",{"ref":"random","offset":0,"limit":50})["items"])==2,"recently added / random list both albums")
al=p.call("browse.list",{"ref":"albums","offset":0,"limit":50}); names=[x["title"] for x in al["items"]]; check(names==["HiRes","Sessions"],"albums "+str(names))
sess=[x for x in al["items"] if x["title"]=="Sessions"][0]; check(sess["artist"]=="Ensemble" and sess["year"]==2021 and sess["art"].startswith("http://127.0.0.1:4533/rest/getCoverArt"),"album fields")
tr=p.call("browse.list",{"ref":sess["ref"],"offset":0,"limit":200}); check([t["track_no"] for t in tr["items"]]==[1,2,3] and tr["items"][0]["format"]=={"sample_rate":44100,"bits":16,"channels":1,"codec":"flac"},"album tracks + format")
pl_ids="&".join("songId="+x["ref"][2:] for x in tr["items"][:2])
urllib.request.urlopen("http://127.0.0.1:4533/rest/createPlaylist?%s&name=Evening%%20mix&%s"%(A,pl_ids)).read()
pg=p.call("browse.list",{"ref":sess["ref"],"offset":1,"limit":1}); check(len(pg["items"])==1 and pg["has_more"] and pg["total"]==3,"paging")
ar=p.call("browse.list",{"ref":"artists","offset":0,"limit":50}); check(sorted(x["title"] for x in ar["items"])==["Ensemble","Trio"],"artists")
tri=[x for x in ar["items"] if x["title"]=="Trio"][0]; check([x["title"] for x in p.call("browse.list",{"ref":tri["ref"],"offset":0,"limit":10})["items"]]==["HiRes"],"artist albums")
sr=p.call("search",{"query":"hi","offset":0,"limit":10}); g={x["kind"]:len(x["items"]) for x in sr["groups"]}; check(g.get("album")==1 and g.get("track")==2 and set(g)=={"artist","album","playlist","track"},"search "+str(g))
sr=p.call("search",{"query":"trio","offset":0,"limit":10}); g={x["kind"]:[i["title"] for i in x["items"]] for x in sr["groups"]}; check(g.get("artist")==["Trio"] and all(i["kind"]=="artist" and i["browsable"] for x in sr["groups"] if x["kind"]=="artist" for i in x["items"]),"search artist "+str(g.get("artist")))
sr=p.call("search",{"query":"evening","offset":0,"limit":10}); pls=[x for x in sr["groups"] if x["kind"]=="playlist"][0]["items"]
check([x["title"] for x in pls]==["Evening mix"] and pls[0]["kind"]=="playlist" and pls[0]["browsable"],"search playlist "+str([x["title"] for x in pls]))
check(p.call("search",{"query":"track","kinds":["track"],"offset":0,"limit":10})["groups"][0]["items"].__len__()==3,"search tracks only")
for m,exp in (("library.albums",2),("library.artists",2),("library.tracks",5),("library.playlists",1)):
    r=p.call(m,{"offset":0,"limit":200}); check(len(r["items"])==exp and "has_more" in r,"%s: %d"%(m,len(r["items"])))
la=p.call("library.albums",{"offset":0,"limit":200})["items"]; check(all(x["kind"]=="album" and x["browsable"] and x.get("artist") and x.get("year") and x.get("art") for x in la),"library.albums: artist, year, art")
lr=p.call("library.artists",{"offset":0,"limit":200})["items"]; check(all(x["kind"]=="artist" and x["browsable"] and x.get("art") for x in lr),"library.artists: art")
lp=p.call("library.playlists",{"offset":0,"limit":200}); pl=lp["items"][0]
check(pl["kind"]=="playlist" and pl["browsable"] and pl["title"]=="Evening mix" and not lp["has_more"],"library.playlists item")
pt=p.call("browse.list",{"ref":pl["ref"],"offset":0,"limit":200})["items"]; check([x["title"] for x in pt]==["Track 1","Track 2"] and all(x["kind"]=="track" for x in pt),"playlist tracks "+str([x["title"] for x in pt]))
t1=tr["items"][0]; check(p.call("item.get",{"ref":t1["ref"]})["title"]=="Track 1","item.get track")
check(p.call("item.get",{"ref":"t/doesnotexist"}).get("code")==-32002,"item.get missing -> not_found")
check(p.call("favorites.set",{"ref":t1["ref"],"on":True}) is None,"star track")
check(p.call("favorites.set",{"ref":sess["ref"],"on":True}) is None,"star album")
fav=p.call("browse.list",{"ref":"favorites","offset":0,"limit":50}); check(sorted(x["kind"] for x in fav["items"])==["album","track"],"favorites list")
p.call("favorites.set",{"ref":sess["ref"],"on":False}); check(len(p.call("browse.list",{"ref":"favorites","offset":0,"limit":50})["items"])==1,"unstar album")
# links, stars, actions
check(t1["album_ref"]==sess["ref"] and t1["artist_ref"]==sess["artist_ref"] and t1["favorite"] is False and sess["artist_ref"].startswith("r/"),"track album_ref / artist_ref")
g=p.call("item.get",{"ref":t1["ref"]}); check(g["favorite"] is True and [a["ref"] for a in g["actions"]]==["rt/"+t1["ref"][2:]],"starred track: favorite, radio action")
check(p.call("browse.list",{"ref":sess["artist_ref"],"offset":0,"limit":10})["items"][0]["title"]=="Sessions","artist_ref browsable")
ens=p.call("item.get",{"ref":sess["artist_ref"]}); acts={a["id"]:a for a in ens["actions"]}
check(set(acts)=={"radio","top","similar"} and acts["radio"]["kind"]=="play" and acts["similar"]["kind"]=="browse","artist actions "+str([a["label"] for a in ens["actions"]]))
for a in ens["actions"]:
    r=p.call("browse.list",{"ref":a["ref"],"offset":0,"limit":50}); f=p.call("item.get",{"ref":a["ref"]})
    check("items" in r and f.get("kind")=="folder" and f["title"].endswith("Ensemble"),"action %s: %d items, item.get %s"%(a["id"],len(r.get("items",[])),f.get("title")))
r=p.call("radio.next",{"seed":t1["ref"],"exclude":[tr["items"][1]["ref"]],"limit":5}); ids=[x["ref"] for x in r.get("items",[])]
check("items" in r and t1["ref"] not in ids and tr["items"][1]["ref"] not in ids and len(ids)<=5,"radio.next: %d tracks (no Last.fm here)"%len(ids))
# lyrics
l=p.call("lyrics.get",{"ref":t1["ref"]}); check(l.get("plain")=="La la la\nLa la","plain lyrics "+json.dumps(l))
l=p.call("lyrics.get",{"ref":tr["items"][1]["ref"]}); check(l.get("synced")==[{"time_ms":1000,"text":"One"},{"time_ms":2500,"text":"Two"}],"synced lyrics "+json.dumps(l))
check(p.call("lyrics.get",{"ref":tr["items"][2]["ref"]}).get("code")==-32002,"no lyrics -> not_found")
# details
d=p.call("item.details",{"ref":sess["ref"]}); facts={f["label"]:f["value"] for f in d.get("facts",[])}
check(facts.get("Genre")=="Jazz" and facts.get("Pistes")=="3","album details facts "+json.dumps(facts,ensure_ascii=False))
d=p.call("item.details",{"ref":sess["artist_ref"]}); check(isinstance(d,dict) and "code" not in d,"artist details "+str(sorted(d)))
check(p.call("item.details",{"ref":t1["ref"]}).get("code")==-32002,"track details -> not_found")
# playlist editing
check(pl["editable"] is True,"own playlist editable")
check([x["entry_id"] for x in pt]==["0:"+pt[0]["ref"][2:],"1:"+pt[1]["ref"][2:]],"entry ids")
new=p.call("playlists.create",{"name":"Morning","description":"d","public":False}); check(new.get("kind")=="playlist" and new["editable"] and new["title"]=="Morning","playlists.create")
nr=new["ref"]; songs=[x["ref"] for x in tr["items"]]
check(p.call("playlists.add",{"ref":nr,"items":songs+[songs[0]]}) is None,"playlists.add")
lst=lambda: p.call("browse.list",{"ref":nr,"offset":0,"limit":50})["items"]
e=lst(); check([x["title"] for x in e]==["Track 1","Track 2","Track 3","Track 1"],"added in order")
check(p.call("playlists.remove",{"ref":nr,"entries":[e[3]["entry_id"],e[1]["entry_id"]]}) is None and [x["title"] for x in lst()]==["Track 1","Track 3"],"playlists.remove (two entries)")
check(p.call("playlists.remove",{"ref":nr,"entries":[e[3]["entry_id"]]}).get("code")==-32602,"stale entry refused")
e=lst(); check(p.call("playlists.move",{"ref":nr,"entry":e[0]["entry_id"],"to":1}) is None and [x["title"] for x in lst()]==["Track 3","Track 1"],"playlists.move")
check(p.call("playlists.add",{"ref":nr,"items":[sess["ref"]]}).get("code")==-32602,"album refs refused in add")
check(p.call("playlists.rename",{"ref":nr,"name":"Dawn"}) is None and p.call("item.get",{"ref":nr})["title"]=="Dawn","playlists.rename")
check(p.call("playlists.delete",{"ref":nr}) is None and p.call("item.get",{"ref":nr}).get("code")==-32002,"playlists.delete")
# direct resolve
r=p.call("track.resolve",{"ref":t1["ref"],"purpose":"play"}); check("format=raw" in r["url"] and r["duration_ms"]==20000 and r.get("replaygain",{}).get("track_gain")==-6.5,"resolve direct "+json.dumps(r.get("replaygain")))
h=urllib.request.urlopen(r["url"]); check(h.headers.get("Content-Length") is not None and h.headers.get("Content-Type")=="audio/flac","direct stream has Content-Length")
# hi-res on a 96k DAC -> transcode
hi=[x for x in p.call("library.tracks",{"offset":0,"limit":200})["items"] if x["title"]=="Hi 1"][0]
r=p.call("track.resolve",{"ref":hi["ref"],"purpose":"play"}); check("getTranscodeStream" in r.get("url","") and r["format"]["sample_rate"]==96000 and r["format"]["bits"]==24,"resolve hi-res -> "+json.dumps(r.get("format",r)))
open(S+"/hi.flac","wb").write(urllib.request.urlopen(r["url"]).read())
pr=subprocess.run(["ffprobe","-v","error","-show_entries","stream=sample_rate,bits_per_raw_sample","-of","csv=p=0",S+"/hi.flac"],capture_output=True,text=True).stdout.strip()
check(pr=="96000,24","transcoded file is "+pr)
p.notify("output.changed",{"output":{"bit_perfect":True,"max_rate":44100,"max_bits":16,"rates":[44100]}}); time.sleep(0.2)
r=p.call("track.resolve",{"ref":hi["ref"],"purpose":"play"}); check(r.get("format",{}).get("sample_rate")==44100 and r["format"]["bits"]==16,"CD DAC: 192k -> "+json.dumps(r.get("format",r)))
# reporting
before=p.call("item.get",{"ref":t1["ref"]})
p.notify("playback.started",{"ref":t1["ref"]}); time.sleep(0.5)
p.notify("playback.ended",{"ref":t1["ref"],"listened_ms":20000,"reason":"ended"}); time.sleep(1.5)
sid=t1["ref"][2:]
pc=json.load(urllib.request.urlopen("http://127.0.0.1:4533/rest/getSong?%s&id=%s"%(A,sid)))["subsonic-response"]["song"].get("playCount",0)
check(pc==1,"scrobble counted a play (playCount=%s)"%pc)
p.notify("playback.started",{"ref":tr["items"][1]["ref"]}); time.sleep(0.3)
p.notify("playback.ended",{"ref":tr["items"][1]["ref"],"listened_ms":3000,"reason":"skipped"}); time.sleep(1)
pc2=json.load(urllib.request.urlopen("http://127.0.0.1:4533/rest/getSong?%s&id=%s"%(A,tr["items"][1]["ref"][2:])))["subsonic-response"]["song"].get("playCount",0)
check(pc2==0,"short skip not counted")
# settings, without a restart
p.notify("settings.changed",{"settings":{"report_playback":False,"transcode":"auto"}}); time.sleep(0.2)
t3=tr["items"][2]; p.notify("playback.started",{"ref":t3["ref"]}); time.sleep(0.3)
p.notify("playback.ended",{"ref":t3["ref"],"listened_ms":20000,"reason":"ended"}); time.sleep(1)
pc3=json.load(urllib.request.urlopen("http://127.0.0.1:4533/rest/getSong?%s&id=%s"%(A,t3["ref"][2:])))["subsonic-response"]["song"].get("playCount",0)
check(pc3==0,"report_playback off: no scrobble")
p.notify("settings.changed",{"settings":{"report_playback":True,"transcode":"never"}}); time.sleep(0.2)
check(p.call("track.resolve",{"ref":hi["ref"],"purpose":"play"}).get("code")==-32003,"transcode never: hi-res unavailable on a CD DAC")
check("format=raw" in p.call("track.resolve",{"ref":t1["ref"],"purpose":"play"})["url"],"transcode never: originals still play")
p.call("shutdown")
# restart: session restored
p=P(); p.call("initialize",{"protocol":1,"data_dir":DATA,"locale":"en","output":OUT})
st=p.call("auth.status"); check(st["state"]=="signed_in","session restored after restart")
# password changed on server -> expired
tok=json.load(open(DATA+"/auth.json")); tok["auth"]["token"]="0"*32; json.dump(tok,open(DATA+"/auth.json","w"))
p.call("shutdown"); p=P(); p.call("initialize",{"protocol":1,"data_dir":DATA,"output":OUT})
e=p.call("browse.list",{"ref":"albums","offset":0,"limit":5}); n=p.notes.get(timeout=5)
check(e.get("code")==-32001 and n["params"]["state"]=="expired","refused token -> expired + auth.changed")
check(p.call("auth.sign_out") is None and not os.path.exists(DATA+"/auth.json") and p.call("auth.status")["state"]=="signed_out","sign out")
check(p.call("nope").get("code")==-32601,"unknown method")
p.call("shutdown")
log=open(S+"/plugin.log").read()
check("admin" not in log and "sesame" not in log and not re.search(r"[?&](u|t|s|p|apiKey)=[^&…\s]",log),"log has no user name nor credentials")
print("FAILURES:",ok); sys.exit(1 if ok else 0)

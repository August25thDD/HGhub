#!/usr/bin/env python3
"""Verify the live Rust service without printing upstream/media URLs or passwords."""
import argparse
import http.cookiejar
import json
import re
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--origin', default='http://127.0.0.1:8900')
parser.add_argument('--password-file', type=Path, default=Path('.clawbench/private/web-password.txt'))
parser.add_argument('--username', default='hghub')
parser.add_argument('--report', type=Path, default=Path('.clawbench/reports/web-live.json'))
args = parser.parse_args()
origin = args.origin.rstrip('/')
report = {'origin': origin, 'checks': {}, 'complete': False}

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

def fetch(opener, path, data=None, content_type=None):
    headers = {'Origin': origin}
    if content_type:
        headers['Content-Type'] = content_type
    req = urllib.request.Request(origin + path, data=data, headers=headers)
    try:
        response = opener.open(req, timeout=90)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, response.headers, response.read()

anonymous = urllib.request.build_opener(NoRedirect)
authenticated = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()), NoRedirect)

def invoke(command, parameters=None):
    status, _, body = fetch(authenticated, '/api/invoke/' + command,
                            json.dumps(parameters or {}).encode(), 'application/json')
    if status != 200:
        # Keep error chains private: they can contain original upstream request URLs.
        private_error = args.report.with_suffix('.private-error.txt')
        private_error.parent.mkdir(parents=True, exist_ok=True)
        private_error.write_bytes(body)
        private_error.chmod(0o600)
        raise AssertionError(f'{command}: HTTP {status}; details saved privately')
    return json.loads(body)

try:
    for path in ['/', '/index.html', '/assets/blocked.js', '/media/hls?u=blocked', '/media/img?u=blocked', '/media/seg?u=blocked']:
        status, headers, _ = fetch(anonymous, path)
        assert status == 303 and headers['Location'] == '/login', f'ungated resource: {path}'
    status, _, _ = fetch(anonymous, '/api/invoke/api_feed', b'{}', 'application/json')
    assert status == 401
    report['checks']['anonymous_gate'] = 'PASS'
    form = urllib.parse.urlencode({'username': args.username, 'password': args.password_file.read_text().strip()}).encode()
    status, headers, _ = fetch(authenticated, '/login', form, 'application/x-www-form-urlencoded')
    assert status == 303 and headers['Location'] == '/', 'password login failed'
    assert 'HttpOnly' in headers['Set-Cookie'] and 'SameSite=Strict' in headers['Set-Cookie']
    report['checks']['password_login'] = 'PASS'
    status, _, html = fetch(authenticated, '/')
    assert status == 200 and b'<div id="root">' in html
    assets = re.findall(rb'(?:src|href)="(/assets/[^\"]+)"', html)
    assert assets
    for asset in assets:
        assert fetch(authenticated, asset.decode())[0] == 200
    report['checks']['authenticated_page_and_assets'] = 'PASS'
    feed = invoke('api_feed', {'page': 1, 'size': 10})
    assert isinstance(feed['items'], list) and feed['items'], 'empty feed'
    report['checks']['real_feed'] = {'status': 200, 'items': len(feed['items'])}
    detail = invoke('api_video', {'id': feed['items'][0]['id']})
    assert detail['id'] == feed['items'][0]['id'] and detail['title'] and detail['episodes']
    report['checks']['real_detail'] = {'status': 200, 'id': detail['id'], 'episodes': len(detail['episodes'])}
    cover = detail.get('cover') or feed['items'][0]['cover']
    status, headers, image = fetch(authenticated, '/media/img?u=' + urllib.parse.quote(cover, safe=''))
    assert status == 200 and headers.get('Content-Type', '').startswith('image/') and len(image) > 100
    report['checks']['cover_proxy'] = {'status': status, 'bytes': len(image), 'type': headers['Content-Type']}
    play = invoke('api_play', {'id': detail['id'], 'ep': detail['episodes'][0]['ep_num']})
    assert play['video_url']
    path = '/media/hls?u=' + urllib.parse.quote(play['video_url'], safe='')
    for _ in range(5):
        status, _, body = fetch(authenticated, path)
        assert status == 200 and body.lstrip(b'\xef\xbb\xbf').startswith(b'#EXTM3U')
        text = body.decode('utf-8-sig')
        uris = re.findall(r'URI="([^\"]+)"', text)
        resources = [line.strip() for line in text.splitlines() if line.strip() and not line.startswith('#')]
        assert resources
        assert all(value.startswith('/media/') for value in uris + resources), 'media bypasses authenticated proxy'
        if '#EXTINF:' in text:
            report['checks']['hls_proxy'] = {'status': status, 'bytes': len(body), 'segments': len(resources)}
            for key in re.findall(r'#EXT-X-KEY:[^\n]*URI="([^\"]+)"', text):
                key_status, _, key_data = fetch(authenticated, key)
                assert key_status == 200 and len(key_data) == 16
                report['checks']['hls_key_proxy'] = {'status': key_status, 'bytes': len(key_data)}
            for segment in resources[:2]:
                seg_status, _, seg_data = fetch(authenticated, segment)
                assert seg_status == 200 and len(seg_data) > 100
                report['checks'].setdefault('video_segments', []).append({'status': seg_status, 'bytes': len(seg_data)})
            break
        path = resources[0]
    else:
        raise AssertionError('no media playlist after five nested playlists')
    status, _, _ = fetch(authenticated, '/logout', b'', 'application/x-www-form-urlencoded')
    assert status == 303
    assert fetch(authenticated, '/')[0] == 303
    report['checks']['logout_revocation'] = 'PASS'
    report['complete'] = True
finally:
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(report, ensure_ascii=False, indent=2))

#!/usr/bin/env python3
"""State/model contracts; no host display or user session is consulted."""
from itertools import product
import json
import unittest
from audit import Aliases, blocks, classify, sanitized_tree, segment_bounds, transform


def window(i, movement, world=None, category='tile', confidence='inferred'):
    return dict(id=str(i), classification=dict(movement=movement, world=world,
                category=category, confidence=confidence))


def raw_window(i=10, flags=0, transient=0, override=False):
    return dict(id=i, attributes_valid=True, metadata_complete=True,
                map_state=2, input_only=False, override_redirect=override,
                flags=flags, transient=transient, geom=[0,0,200,100,0])


def logical(**kw):
    r=dict(world=[0,0],active=True,column=0,container='column',float=False,
           fullscreen=False,maximized=False,sticky=False,overlay=False)
    r.update(kw)
    return r


class AuditTests(unittest.TestCase):
    def test_prompt_example_has_three_distinct_blocks(self):
        a=[window(i,'tile' if c=='T' else 'independent',[0,0] if c=='T' else None,
                  'tile' if c=='T' else 'override_redirect') for i,c in enumerate('TTETTET')]
        m=blocks(a,[0,0]);self.assertEqual(m['lengths'],[2,2,1]);self.assertEqual(m['segments'],3)
        self.assertEqual(m['interruptions'],2);self.assertEqual(m['maximum_block'],2)
        self.assertEqual(m['principal_fraction'],2/5);self.assertEqual(m['groupable_tile_fraction'],4/5)
        self.assertEqual([[s['id'] for s in x] for x in m['separators']],[['2'],['5']])

    def test_segment_bounds_match_exhaustive_unknown_assignments(self):
        for n in range(7):
            for pattern in product('TEU',repeat=n):
                seq=[window(i,{'T':'tile','E':'independent','U':'unknown'}[c],[0,0] if c=='T' else None) for i,c in enumerate(pattern)]
                values=[]
                for substitutions in product([0,1],repeat=pattern.count('U')):
                    it=iter(substitutions);bits=[next(it) if c=='U' else int(c=='T') for c in pattern]
                    values.append(sum(b and (i==0 or not bits[i-1]) for i,b in enumerate(bits)))
                self.assertEqual(segment_bounds(seq,[0,0]),dict(minimum=min(values),maximum=max(values)))

    def test_worlds_cannot_merge(self):
        seq=[window(1,'tile',[0,0]),window(2,'tile',[1,0]),window(3,'tile',[0,0])]
        self.assertEqual(blocks(seq,[0,0])['segments'],2)
        self.assertEqual(blocks(seq,[1,0])['segments'],1)

    def test_unknown_never_proves_compatibility(self):
        seq=[window(1,'tile',[0,0]),window(2,'unknown'),window(3,'tile',[0,0])]
        m=blocks(seq,[0,0]);self.assertEqual(m['unknown_assignment_bounds'],dict(minimum=1,maximum=2))
        self.assertFalse(m['actual_architecture_compatibility_proved']);self.assertFalse(m['stacking_model_fully_classified'])
        strict=blocks(seq,[0,0],True);self.assertEqual(strict['tiles'],0);self.assertEqual(strict['unknown'],3)

    def test_fullscreen_membership_is_not_automatically_eligible(self):
        for bits in [dict(fullscreen=True),dict(maximized=True),dict(overlay=True),dict(sticky=True)]:
            c=classify(raw_window(),logical(**bits),True,True,True)
            self.assertEqual(c['movement'],'unknown')

    def test_inactive_float_and_transient_are_not_grouped(self):
        c=classify(raw_window(),logical(active=False),True,True,True)
        self.assertEqual(c['movement'],'independent');self.assertEqual(c['category'],'tile')
        c=classify(raw_window(transient=9),logical(container='float',float=True),True,True,True)
        self.assertEqual((c['category'],c['movement']),('transient','independent'))
        c=classify(raw_window(transient=9),logical(),True,True,True)
        self.assertEqual(c['movement'],'unknown')

    def test_racy_metadata_and_missing_ipc_are_unknown(self):
        for coherent in [False,True]:
            w=raw_window();w['metadata_complete']=False
            self.assertEqual(classify(w,logical(),coherent,True,True)['movement'],'unknown')
        self.assertEqual(classify(raw_window(),logical(),False,True,True)['movement'],'unknown')
        self.assertEqual(classify(raw_window(),None,False,True,True)['movement'],'unknown')
        self.assertEqual(classify(raw_window(override=True),None,False,False,False)['movement'],'independent')

    def test_missing_policy_field_is_unknown(self):
        l=logical();l['overlay']=None
        self.assertEqual(classify(raw_window(),l,True,True,True)['movement'],'unknown')

    def test_ids_are_local_and_reuse_creates_a_new_alias(self):
        p=Aliases(1);first=p.get(1234)
        e=p.event(dict(window=1234,kind=4,t_ns=20));self.assertEqual(e['window'],first)
        self.assertNotEqual(p.get(1234),first);self.assertIsNone(p.get(0))

    def test_private_fields_are_not_copied(self):
        w=dict(id=10,title='PRIVATE_TITLE',**logical());w['class']='PRIVATE_CLASS';w['pid']=999999
        document=dict(monitors=[dict(index=0,active_ws=0,focused_title='PRIVATE_FOCUS',workspaces=[dict(index=0,scroll=0,columns=[dict(windows=[w])],floats=[])])])
        logical_state=sanitized_tree(document)
        snapshot=dict(windows=[raw_window()],managed=[10],published_stack=[10],managed_valid=True,stacking_valid=True,events=[],root_stable=True)
        out=transform(snapshot,logical_state,True,Aliases(1))
        encoded=json.dumps(out)
        self.assertNotIn('PRIVATE',encoded);self.assertNotIn('999999',encoded)
        self.assertNotIn('title',encoded);self.assertNotIn('pid',encoded)

    def test_duration_is_not_snapshot_count(self):
        from analyze import intervals
        rows=[dict(controller_end_ns=2,label='one'),dict(controller_end_ns=3,label='one'),dict(controller_end_ns=8,label='two')]
        result=list(intervals(rows,10))
        self.assertEqual([dt for _,dt in result],[1,5,2])
        self.assertEqual(sum(dt for row,dt in result if row['label']=='one'),6)
        self.assertEqual(sum(dt for row,dt in result if row['label']=='two'),2)
        with self.assertRaises(ValueError):list(intervals(list(reversed(rows)),10))

    def test_exclusive_overlay_can_be_confirmed_independent(self):
        c=classify(raw_window(flags=1),logical(fullscreen=True,overlay=True),True,True,True)
        self.assertEqual((c['category'],c['movement'],c['confidence']),('presentation','independent','confirmed'))

    def test_published_dock_does_not_fake_managed_order_disagreement(self):
        snapshot=dict(windows=[raw_window(10),raw_window(11,flags=16)],managed=[10],published_stack=[10,11],managed_valid=True,stacking_valid=True,events=[],root_stable=True)
        state=dict(worlds=[dict(monitor=0,workspace=0,camera=0)],windows={10:logical()})
        out=transform(snapshot,state,True,Aliases(1))
        self.assertTrue(out['published_managed_order_agrees']);self.assertTrue(out['published_order_is_root_subsequence'])

    def test_root_direction_changes_expected_window_order(self):
        seq=[window(1,'tile',[0,0]),window(2,'tile',[0,0]),window(3,'independent')]
        self.assertEqual(blocks(seq,[0,0])['blocks'],[['1','2']])
        self.assertNotEqual(blocks(list(reversed(seq)),[0,0])['blocks'],[['1','2']])


if __name__=='__main__':
    unittest.main()
